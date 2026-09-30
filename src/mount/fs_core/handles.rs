//! Open handle table. No I/O.
use super::error::{FsError, FsResult};
use super::identity::FileId;
use crate::mount::virtual_drive::Revision;
use crate::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct HandleId(pub(crate) u64);

#[derive(Clone)]
pub(super) enum View {
    /// Immutable revision captured at open.
    Snapshot(Revision),
    /// Reads the file's unsealed generation when there is one, else `base`,
    /// which this core moves to each revision it seals for the file.
    Attached { base: Option<Revision> },
}

#[derive(Clone)]
pub(super) struct Handle {
    pub(super) file: FileId,
    pub(super) write: bool,
    pub(super) append: bool,
    pub(super) view: View,
}

#[derive(Default)]
pub(super) struct HandleTable {
    next: u64,
    open: BTreeMap<HandleId, Handle>,
}
impl HandleTable {
    pub(super) fn insert(&mut self, handle: Handle) -> HandleId {
        self.next += 1;
        let id = HandleId(self.next);
        self.open.insert(id, handle);
        id
    }
    pub(super) fn get(&self, id: HandleId) -> FsResult<Handle> {
        self.open.get(&id).cloned().ok_or(FsError::BadHandle)
    }
    pub(super) fn remove(&mut self, id: HandleId) -> FsResult<Handle> {
        self.open.remove(&id).ok_or(FsError::BadHandle)
    }
    pub(super) fn writers(&self, file: FileId) -> usize {
        self.open
            .values()
            .filter(|h| h.file == file && h.write)
            .count()
    }
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
