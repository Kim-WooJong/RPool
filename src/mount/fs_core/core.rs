//! `FsCore`: open, read, write, truncate and release over shared generations.
//!
//! Lock order: one slot's start lock → `namespace` → one generation slot →
//! drive locks. The id, handle, slot-map and slot-summary locks are leaves:
//! never lock a slot while holding one of them. A seal's fsync and full hash
//! (`prehash`) and a new generation's baseline copy (`mutate`) run with no
//! namespace or generation lock held, so a large file's close or first write
//! never blocks lookups, listings or namespace changes.
use super::error::{FsError, FsResult};
use super::generation::Generation;
use super::handles::{Handle, HandleId, HandleTable, View};
use super::identity::{FileId, IdTable};
use super::slot::Slot;
use crate::mount::namespace::valid_path;
use crate::mount::native_ancestry::Ancestry;
use crate::mount::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;
use std::sync::RwLock;

/// Failed unlocked baseline copies (raced by a rename, delete or new base)
/// before a start copies under the locks, which always completes.
const UNLOCKED_STARTS: usize = 3;

/// What a new generation starts from; equal plans start equal generations.
struct Plan {
    path: String,
    base: Option<Revision>,
    ancestry: Option<Ancestry>,
}
impl Plan {
    fn copies(&self, keep: u64) -> bool {
        self.base.as_ref().is_some_and(|b| keep.min(b.size()) > 0)
    }
    fn same(&self, other: &Plan) -> bool {
        self.path == other.path
            && self.base.as_ref().map(Revision::id) == other.base.as_ref().map(Revision::id)
            && self.ancestry == other.ancestry
    }
    fn start(&self, drive: &VirtualDrive, keep: u64) -> Result<Generation> {
        Generation::start(
            drive,
            &self.path,
            self.base.as_ref(),
            keep,
            self.ancestry.as_ref(),
        )
    }
}

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
    /// Pool-sync workspaces (and the local-only test fixture).
    pub(crate) fn new(drive: Arc<VirtualDrive>) -> FsResult<Self> {
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
        Ok(self.drive.visible_revision(path)?)
    }
    /// Whether the file has unsealed writes; `None` when it has no slot.
    pub(super) fn unsealed_size(&self, file: FileId) -> FsResult<Option<(u64, String)>> {
        let slot = lock(&self.slots)?.get(&file).cloned();
        let Some(slot) = slot else { return Ok(None) };
        slot.summary()
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
            let mut generation = slot.lock()?;
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
            if let Some(generation) = slot.lock()?.as_mut() {
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
    /// `keep` bytes of the file's current revision when there is none.
    ///
    /// The baseline copy of a new generation runs holding only this file's
    /// start lock, so other files' operations proceed (same-file writers wait
    /// on the start lock). The prepared generation is installed only if,
    /// under the locks again, nothing changed what it would start from (a
    /// rename, delete, new revision or concurrent truncating open); otherwise
    /// it is discarded and the start is planned again.
    fn mutate(
        &self,
        handle: HandleId,
        keep: u64,
        change: impl FnOnce(&mut Generation, &VirtualDrive) -> Result<()>,
    ) -> FsResult<()> {
        let file = lock(&self.handles)?.get(handle)?.file;
        let mut change = Some(change);
        let mut apply = |generation: &mut Generation| -> FsResult<()> {
            let change = change.take().expect("a mutation applies once");
            Ok(change(generation, &self.drive)?)
        };
        let mut attempts = 0;
        loop {
            let slot = self.slot(file)?;
            let _starting = slot.starting()?;
            // While started, the slot cannot be dropped (`SlotCell::idle`).
            if !Arc::ptr_eq(&slot, &self.slot(file)?) {
                continue;
            }
            let plan = {
                let _namespace = self.shared()?;
                let mut generation = slot.lock()?;
                // Re-read under the slot lock: a concurrent seal has finished rebasing.
                let handle = self.writer(handle)?;
                if let Some(generation) = generation.as_mut() {
                    return apply(generation);
                }
                let plan = self.plan(file, &handle, keep)?;
                if !plan.copies(keep) || attempts >= UNLOCKED_STARTS {
                    let generation = generation.insert(plan.start(&self.drive, keep)?);
                    return apply(generation);
                }
                plan
            };
            // The slow part, holding only the start lock.
            let prepared = plan.start(&self.drive, keep)?;
            let stale = {
                let _namespace = self.shared()?;
                let mut generation = slot.lock()?;
                let current = generation.is_none()
                    && self
                        .writer(handle)
                        .and_then(|h| self.plan(file, &h, keep))
                        .is_ok_and(|now| now.same(&plan));
                if current {
                    let generation = generation.insert(prepared);
                    return apply(generation);
                }
                prepared
            };
            // Never acknowledged; the next round reports any lasting error.
            let _ = stale.discard(&self.drive);
            attempts += 1;
        }
    }
    /// The handle, which must be a write handle.
    fn writer(&self, handle: HandleId) -> FsResult<Handle> {
        let handle = lock(&self.handles)?.get(handle)?;
        if !handle.write {
            return Err(FsError::ReadOnly);
        }
        Ok(handle)
    }
    /// What a new generation of `file` starts from, for a change keeping the
    /// first `keep` bytes.
    fn plan(&self, file: FileId, handle: &Handle, keep: u64) -> FsResult<Plan> {
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
        Ok(Plan {
            path,
            base,
            ancestry,
        })
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

    /// Whether `handle` is an open write handle (whose close may seal).
    pub(crate) fn writes(&self, handle: HandleId) -> bool {
        lock(&self.handles).is_ok_and(|h| h.get(handle).is_ok_and(|h| h.write))
    }

    /// Closes a handle. The last write handle seals the file's unsealed writes;
    /// an unlinked file's unsealed writes are discarded instead.
    /// The seal's flush and hash run before the namespace lock is taken
    /// (`prehash`), so other operations are not held up by a large file.
    pub(crate) fn release(&self, handle: HandleId) -> FsResult<()> {
        // Best guess at whether this closes the last writer; `finish` decides
        // under the lock and re-hashes itself if a write raced this hash.
        let closing = lock(&self.handles)?.get(handle)?;
        let hashed = if closing.write && lock(&self.handles)?.writers(closing.file) == 1 {
            self.prehash(closing.file)
        } else {
            Ok(())
        };
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
            result = self.finish(released.file, hashed);
        }
        if !remaining {
            let mut slots = lock(&self.slots)?;
            if slots.get(&released.file).is_some_and(|slot| slot.idle()) {
                slots.remove(&released.file);
            }
        }
        result
    }

    /// `hashed` is the result of this release's `prehash`; its failure is a
    /// seal failure (as when the seal itself flushed and hashed).
    fn finish(&self, file: FileId, hashed: FsResult<()>) -> FsResult<()> {
        let linked = lock(&self.ids)?.path(file).is_some();
        if linked {
            let sealed = hashed.and_then(|()| self.seal_file(file));
            if sealed.is_err() {
                // No writer is left to retry. Forget the unacknowledged
                // generation; its spool stays on disk for recovery.
                let slot = lock(&self.slots)?.get(&file).cloned();
                if let Some(slot) = slot {
                    drop(slot.lock()?.take());
                }
            }
            return sealed;
        }
        let slot = lock(&self.slots)?.get(&file).cloned();
        if let Some(slot) = slot {
            let taken = slot.lock()?.take();
            if let Some(generation) = taken {
                generation.discard(&self.drive)?;
            }
        }
        Ok(())
    }
}
