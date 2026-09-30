//! `FsCore`: open, read, write, truncate and release over shared generations.
//!
//! Lock order: `namespace` → one generation slot → drive locks. The id, handle
//! and slot-map locks are leaves: never lock a slot while holding one of them.
use super::error::{FsError, FsResult};
use super::generation::Generation;
use super::handles::{Handle, HandleId, HandleTable, View};
use super::identity::{FileId, IdTable};
use crate::mount::namespace::valid_path;
use crate::mount::native_ancestry::Ancestry;
use crate::mount::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;
use std::sync::RwLock;

pub(super) type Slot = Arc<Mutex<Option<Generation>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    Write { truncate: bool, append: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attr {
    pub(crate) id: Option<FileId>,
    pub(crate) size: u64,
    pub(crate) directory: bool,
    /// Revision or intent id; changes whenever content changes.
    pub(crate) tag: String,
}

pub(crate) struct FsCore {
    pub(super) drive: Arc<VirtualDrive>,
    /// Shared by data operations, exclusive for namespace changes.
    pub(super) namespace: RwLock<()>,
    pub(super) ids: Mutex<IdTable>,
    pub(super) handles: Mutex<HandleTable>,
    pub(super) slots: Mutex<BTreeMap<FileId, Slot>>,
    /// Pool-sync workspace: peers change files, so handles keep the revision
    /// they started from and edits descend from what was actually read.
    pub(super) peer: bool,
    /// The revision each file's handles last read (or this core last sealed).
    pub(super) observed: Mutex<BTreeMap<FileId, Revision>>,
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> FsResult<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| FsError::Io(anyhow!("filesystem core lock poisoned")))
}
pub(super) fn checked(path: &str) -> FsResult<()> {
    valid_path(path).map_err(|_| FsError::InvalidPath)
}

impl FsCore {
    /// Local and pool-sync (v6) workspaces. v7 private snapshots and the
    /// bounded shared protocol stay on the DAV route.
    pub(crate) fn new(drive: Arc<VirtualDrive>) -> FsResult<Self> {
        if drive.peer_retention || drive.bounded_shared {
            return Err(FsError::Io(anyhow!(
                "the native frontend does not support v7 history or bounded shared workspaces yet"
            )));
        }
        let peer = !drive.pool_sync_roots.is_empty();
        Ok(Self {
            drive,
            namespace: RwLock::new(()),
            ids: Mutex::new(IdTable::default()),
            handles: Mutex::new(HandleTable::default()),
            slots: Mutex::new(BTreeMap::new()),
            peer,
            observed: Mutex::new(BTreeMap::new()),
        })
    }
    /// Records the revision a handle of `file` read; cloud reads persist it
    /// as the path's base so later edits descend from it.
    pub(super) fn remember(&self, file: FileId, revision: &Revision) -> FsResult<()> {
        if !self.peer {
            return Ok(());
        }
        let changed = lock(&self.observed)?
            .insert(file, revision.clone())
            .is_none_or(|previous| previous.id() != revision.id());
        if let (true, Revision::Cloud { id, .. }) = (changed, revision) {
            if let Some(path) = lock(&self.ids)?.path(file) {
                self.drive.observe_read(&path, id)?;
            }
        }
        Ok(())
    }
    /// Ancestry for an edit that does not keep existing bytes (truncate,
    /// create, delete): the last revision read, else what this workspace last
    /// did at the path (see `VirtualDrive::unread_ancestry`).
    pub(super) fn read_ancestry(&self, file: FileId, path: &str) -> FsResult<Ancestry> {
        if let Some(read) = lock(&self.observed)?.get(&file) {
            return Ok(Ancestry::of(read));
        }
        let exists = self.visible(path)?.is_some();
        Ok(self.drive.unread_ancestry(path, exists)?)
    }
    pub(super) fn shared(&self) -> FsResult<std::sync::RwLockReadGuard<'_, ()>> {
        self.namespace
            .read()
            .map_err(|_| FsError::Io(anyhow!("filesystem core lock poisoned")))
    }
    pub(super) fn exclusive(&self) -> FsResult<std::sync::RwLockWriteGuard<'_, ()>> {
        self.namespace
            .write()
            .map_err(|_| FsError::Io(anyhow!("filesystem core lock poisoned")))
    }
    pub(super) fn slot(&self, file: FileId) -> FsResult<Slot> {
        Ok(lock(&self.slots)?.entry(file).or_default().clone())
    }
    pub(super) fn visible(&self, path: &str) -> FsResult<Option<Revision>> {
        Ok(self.drive.view()?.get(path).cloned())
    }
    /// Whether the file has unsealed writes; `None` when it has no slot.
    pub(super) fn unsealed_size(&self, file: FileId) -> FsResult<Option<(u64, String)>> {
        let slot = lock(&self.slots)?.get(&file).cloned();
        let Some(slot) = slot else { return Ok(None) };
        let generation = lock(&slot)?;
        Ok(generation.as_ref().map(|g| (g.size, g.intent.id.clone())))
    }

    pub(crate) fn open(
        &self,
        path: &str,
        access: Access,
        create: bool,
        exclusive: bool,
    ) -> FsResult<HandleId> {
        checked(path)?;
        let _namespace = self.shared()?;
        let visible = self.visible(path)?;
        let file = lock(&self.ids)?.id(path);
        let unsealed = self.unsealed_size(file)?.is_some();
        let exists = visible.is_some() || unsealed;
        if !exists && self.lookup(path).is_ok_and(|attr| attr.directory) {
            return Err(FsError::IsDir);
        }
        if exists && exclusive {
            return Err(FsError::Exists);
        }
        if !exists && !(create && access != Access::Read) {
            return Err(FsError::NotFound);
        }
        if let Some(revision) = &visible {
            if self.peer {
                // Retention protection only; ancestry comes from actual reads.
                self.drive.observe_open(path, revision);
            } else {
                // Persist ancestry and keep cloud bytes from retention while served.
                self.drive.pin_read(path, revision)?;
            }
        }
        // Attached handles of one file share a base; a peer revision reaches
        // them only after all of them are closed.
        let joined = if self.peer {
            lock(&self.handles)?.attached_base(file)
        } else {
            None
        };
        let (write, append, truncate) = match access {
            Access::Read => (false, false, false),
            Access::Write { truncate, append } => (true, append, truncate),
        };
        if write && (truncate || !exists) {
            let slot = self.slot(file)?;
            let mut generation = lock(&slot)?;
            match generation.as_mut() {
                Some(g) => g.truncate(&self.drive, 0)?,
                None => {
                    let ancestry = if self.peer {
                        Some(self.read_ancestry(file, path)?)
                    } else {
                        None
                    };
                    *generation = Some(Generation::start(
                        &self.drive,
                        path,
                        visible.as_ref(),
                        0,
                        ancestry.as_ref(),
                    )?)
                }
            }
        }
        let view = match (&visible, write || unsealed) {
            (Some(revision), false) => View::Snapshot(revision.clone()),
            _ => View::Attached {
                base: joined.unwrap_or(visible),
            },
        };
        Ok(lock(&self.handles)?.insert(Handle {
            file,
            write,
            append,
            view,
        }))
    }

    pub(crate) fn read_at(&self, handle: HandleId, offset: u64, count: usize) -> FsResult<Vec<u8>> {
        let handle = lock(&self.handles)?.get(handle)?;
        let base = match handle.view {
            View::Snapshot(revision) => {
                self.remember(handle.file, &revision)?;
                return Ok(self.drive.read(&revision, offset, count)?);
            }
            View::Attached { base } => base,
        };
        let slot = lock(&self.slots)?.get(&handle.file).cloned();
        if let Some(slot) = slot {
            if let Some(generation) = lock(&slot)?.as_mut() {
                return Ok(generation.read_at(offset, count)?);
            }
        }
        // Local: a linked file reads its current revision, an unlinked one its
        // last. Pool sync: the shared base, so a peer update never mixes in.
        let current = match (self.peer, lock(&self.ids)?.path(handle.file)) {
            (false, Some(path)) => self.visible(&path)?,
            _ => base,
        };
        match current {
            Some(revision) => {
                self.remember(handle.file, &revision)?;
                Ok(self.drive.read(&revision, offset, count)?)
            }
            None => Ok(vec![]),
        }
    }

    /// Attributes of an open handle's file as that handle sees it, including
    /// after an unlink (for fstat-style queries).
    pub(crate) fn stat(&self, handle: HandleId) -> FsResult<Attr> {
        let handle = lock(&self.handles)?.get(handle)?;
        let unsealed = match handle.view {
            View::Snapshot(_) => None,
            View::Attached { .. } => self.unsealed_size(handle.file)?,
        };
        if let Some((size, tag)) = unsealed {
            return Ok(Attr {
                id: Some(handle.file),
                size,
                directory: false,
                tag,
            });
        }
        let revision = match handle.view {
            View::Snapshot(revision) => Some(revision),
            View::Attached { base } => match (self.peer, lock(&self.ids)?.path(handle.file)) {
                (false, Some(path)) => self.visible(&path)?,
                _ => base,
            },
        };
        Ok(Attr {
            id: Some(handle.file),
            size: revision.as_ref().map_or(0, |r| r.size()),
            directory: false,
            tag: revision.map(|r| r.id().to_string()).unwrap_or_default(),
        })
    }

    /// Runs `change` on the file's generation, starting one with the first
    /// `keep` bytes of the file's current revision when there is none. The
    /// revision is read under the slot lock, after any concurrent seal.
    fn mutate(
        &self,
        handle: HandleId,
        keep: u64,
        change: impl FnOnce(&mut Generation, &VirtualDrive) -> Result<()>,
    ) -> FsResult<()> {
        let _namespace = self.shared()?;
        let file = lock(&self.handles)?.get(handle)?.file;
        let slot = self.slot(file)?;
        let mut generation = lock(&slot)?;
        // Re-read under the slot lock: a concurrent seal has finished rebasing.
        let handle = lock(&self.handles)?.get(handle)?;
        if !handle.write {
            return Err(FsError::ReadOnly);
        }
        if generation.is_none() {
            let path = lock(&self.ids)?.path(file).ok_or(FsError::Stale)?;
            let (base, ancestry) = if self.peer {
                // The bytes kept and the recorded ancestry are the same revision.
                let base = match &handle.view {
                    View::Attached { base } => base.clone(),
                    View::Snapshot(revision) => Some(revision.clone()),
                };
                if keep > 0 {
                    let ancestry = base.as_ref().map(Ancestry::of).unwrap_or(Ancestry::Default);
                    (base, Some(ancestry))
                } else {
                    (self.visible(&path)?, Some(self.read_ancestry(file, &path)?))
                }
            } else {
                (self.visible(&path)?, None)
            };
            *generation = Some(Generation::start(
                &self.drive,
                &path,
                base.as_ref(),
                keep,
                ancestry.as_ref(),
            )?);
        }
        let generation = generation.as_mut().expect("generation started above");
        Ok(change(generation, &self.drive)?)
    }

    /// Writes all of `bytes` or fails. As with POSIX, a failed write may leave
    /// a prefix in the unsealed generation that a later `fsync` seals.
    pub(crate) fn write_at(&self, handle: HandleId, offset: u64, bytes: &[u8]) -> FsResult<usize> {
        let append = lock(&self.handles)?.get(handle)?.append;
        self.mutate(handle, u64::MAX, |generation, drive| {
            let offset = if append { generation.size } else { offset };
            offset
                .checked_add(bytes.len() as u64)
                .context("write range overflow")?;
            generation.write_at(drive, offset, bytes)
        })?;
        Ok(bytes.len())
    }

    pub(crate) fn truncate(&self, handle: HandleId, len: u64) -> FsResult<()> {
        self.mutate(handle, len, |generation, drive| {
            generation.truncate(drive, len)
        })
    }

    /// Closes a handle. The last write handle seals the file's unsealed writes;
    /// an unlinked file's unsealed writes are discarded instead.
    pub(crate) fn release(&self, handle: HandleId) -> FsResult<()> {
        let _namespace = self.shared()?;
        let (released, writers, remaining) = {
            let mut handles = lock(&self.handles)?;
            let released = handles.remove(handle)?;
            (
                released.clone(),
                handles.writers(released.file),
                handles.any(released.file),
            )
        };
        let mut result = Ok(());
        if released.write && writers == 0 {
            result = self.finish(released.file);
        }
        if !remaining {
            let mut slots = lock(&self.slots)?;
            if slots
                .get(&released.file)
                .is_some_and(|slot| slot.try_lock().is_ok_and(|g| g.is_none()))
            {
                slots.remove(&released.file);
            }
        }
        result
    }

    fn finish(&self, file: FileId) -> FsResult<()> {
        let linked = lock(&self.ids)?.path(file).is_some();
        if linked {
            let sealed = self.seal_file(file);
            if sealed.is_err() {
                // No writer is left to retry. Forget the unacknowledged
                // generation; its spool stays on disk for recovery.
                let slot = lock(&self.slots)?.get(&file).cloned();
                if let Some(slot) = slot {
                    drop(lock(&slot)?.take());
                }
            }
            return sealed;
        }
        let slot = lock(&self.slots)?.get(&file).cloned();
        if let Some(slot) = slot {
            if let Some(generation) = lock(&slot)?.take() {
                generation.discard(&self.drive)?;
            }
        }
        Ok(())
    }
}
