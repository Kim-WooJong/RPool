//! Open handle table. No I/O.
use super::error::{FsError, FsResult};
use super::identity::FileId;
use crate::mount::virtual_drive::Revision;
use crate::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Opaque open-handle number given to frontends (FUSE `fh`, WinFsp context).
pub(crate) struct HandleId(pub(crate) u64);

#[derive(Clone)]
/// What an open handle reads.
pub(super) enum View {
    /// Immutable revision captured at open.
    Snapshot(Revision),
    /// Reads the file's unsealed generation when there is one, else `base`,
    /// which this core moves to each revision it seals for the file.
    Attached {
        /// Revision read when the file has no unsealed generation; `None` when the file has
        /// no revision yet (created by this open).
        base: Option<Revision>,
    },
}

#[derive(Clone)]
/// One open handle in the table.
pub(super) struct Handle {
    /// File the handle refers to (follows renames).
    pub(super) file: FileId,
    /// Opened for writing.
    pub(super) write: bool,
    /// Writes go to the end of the file.
    pub(super) append: bool,
    /// What reads see.
    pub(super) view: View,
}

#[derive(Default)]
/// Open handles of one `FsCore`.
pub(super) struct HandleTable {
    /// Last handle id handed out (ids are never reused).
    next: u64,
    /// Open handles by id.
    open: BTreeMap<HandleId, Handle>,
}
impl HandleTable {
    /// Adds `handle` and returns its new id.
    pub(super) fn insert(&mut self, handle: Handle) -> HandleId {
        self.next += 1;
        let id = HandleId(self.next);
        self.open.insert(id, handle);
        id
    }
    /// Copy of an open handle, `BadHandle` if unknown.
    pub(super) fn get(&self, id: HandleId) -> FsResult<Handle> {
        self.open.get(&id).cloned().ok_or(FsError::BadHandle)
    }
    /// Removes and returns a handle, `BadHandle` if unknown.
    pub(super) fn remove(&mut self, id: HandleId) -> FsResult<Handle> {
        self.open.remove(&id).ok_or(FsError::BadHandle)
    }
    /// Number of open write handles of `file`.
    pub(super) fn writers(&self, file: FileId) -> usize {
        self.open
            .values()
            .filter(|h| h.file == file && h.write)
            .count()
    }
    /// True if any handle of `file` is open.
    pub(super) fn any(&self, file: FileId) -> bool {
        self.open.values().any(|h| h.file == file)
    }
    /// The base shared by the file's open attached handles, if any.
    pub(super) fn attached_base(&self, file: FileId) -> Option<Option<Revision>> {
        self.open.values().find_map(|h| match &h.view {
            View::Attached { base } if h.file == file => Some(base.clone()),
            _ => None,
        })
    }
    /// After a seal, attached handles of the file read the sealed revision.
    pub(super) fn rebase(&mut self, file: FileId, revision: Option<Revision>) {
        for handle in self.open.values_mut().filter(|h| h.file == file) {
            if let View::Attached { base } = &mut handle.view {
                *base = revision.clone();
            }
        }
    }
}
