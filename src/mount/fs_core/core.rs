//! `FsCore`: open, read, write, truncate and release over shared generations.
//!
//! Lock order: `namespace` → one generation slot → drive locks. The id, handle
//! and slot-map locks are leaves: never lock a slot while holding one of them.
use super::error::{FsError, FsResult};
use super::generation::Generation;
use super::handles::{Handle, HandleId, HandleTable, View};
use super::identity::{FileId, IdTable};
use crate::mount::namespace::valid_path;
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
    /// Shared-history and pool-sync workspaces stay on the DAV route until
    /// traces cover them.
    pub(crate) fn new(drive: Arc<VirtualDrive>) -> FsResult<Self> {
        if drive.peer_retention || drive.bounded_shared || !drive.pool_sync_roots.is_empty() {
            return Err(FsError::Io(anyhow!(
                "filesystem core supports local workspaces only for now"
            )));
        }
        Ok(Self {
            drive,
            namespace: RwLock::new(()),
            ids: Mutex::new(IdTable::default()),
            handles: Mutex::new(HandleTable::default()),
            slots: Mutex::new(BTreeMap::new()),
        })
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
            // Persist ancestry and keep cloud bytes from retention while served.
            self.drive.pin_read(path, revision)?;
        }
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
                    *generation = Some(Generation::start(&self.drive, path, visible.as_ref(), 0)?)
                }
            }
        }
        let view = match (&visible, write || unsealed) {
            (Some(revision), false) => View::Snapshot(revision.clone()),
            _ => View::Attached { base: visible },
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
            View::Snapshot(revision) => return Ok(self.drive.read(&revision, offset, count)?),
            View::Attached { base } => base,
        };
        let slot = lock(&self.slots)?.get(&handle.file).cloned();
        if let Some(slot) = slot {
            if let Some(generation) = lock(&slot)?.as_mut() {
                return Ok(generation.read_at(offset, count)?);
            }
        }
        // A linked file reads its current revision; an unlinked one its last.
        let current = match lock(&self.ids)?.path(handle.file) {
            Some(path) => self.visible(&path)?,
            None => base,
        };
        match current {
            Some(revision) => Ok(self.drive.read(&revision, offset, count)?),
            None => Ok(vec![]),
        }
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
        let handle = lock(&self.handles)?.get(handle)?;
        if !handle.write {
            return Err(FsError::ReadOnly);
        }
        let slot = self.slot(handle.file)?;
        let mut generation = lock(&slot)?;
        if generation.is_none() {
            let path = lock(&self.ids)?.path(handle.file).ok_or(FsError::Stale)?;
            let base = self.visible(&path)?;
            *generation = Some(Generation::start(&self.drive, &path, base.as_ref(), keep)?);
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
            return self.seal_file(file);
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
