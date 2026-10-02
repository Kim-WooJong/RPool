//! Per-session file identity. A `FileId` follows its file across renames and
//! outlives unlink while handles still refer to it. Not persisted.
use crate::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Session-local identity of a file (not an inode number, not persisted).
pub(crate) struct FileId(pub(crate) u64);

#[derive(Default)]
/// Path ↔ [`FileId`] table of one `FsCore`.
pub(super) struct IdTable {
    /// Last id handed out (ids are never reused).
    next: u64,
    /// Id of each linked path.
    by_path: BTreeMap<String, FileId>,
    /// Path of each linked id.
    paths: BTreeMap<FileId, String>,
}
impl IdTable {
    /// The file's id, allocating one on first sight.
    pub(super) fn id(&mut self, path: &str) -> FileId {
        if let Some(id) = self.by_path.get(path) {
            return *id;
        }
        self.next += 1;
        let id = FileId(self.next);
        self.by_path.insert(path.into(), id);
        self.paths.insert(id, path.into());
        id
    }
    /// Id of `path` without allocating one.
    pub(super) fn get(&self, path: &str) -> Option<FileId> {
        self.by_path.get(path).copied()
    }
    /// `None` once the file was unlinked or replaced.
    pub(super) fn path(&self, id: FileId) -> Option<String> {
        self.paths.get(&id).cloned()
    }
    /// Detaches `path` from its id and returns the id; the id stays valid for open handles.
    pub(super) fn unlink(&mut self, path: &str) -> Option<FileId> {
        let id = self.by_path.remove(path)?;
        self.paths.remove(&id);
        Some(id)
    }
    /// Move a file's identity; a replaced destination becomes unlinked.
    pub(super) fn rename(&mut self, from: &str, to: &str) {
        self.unlink(to);
        if let Some(id) = self.unlink(from) {
            self.by_path.insert(to.into(), id);
            self.paths.insert(id, to.into());
        }
    }
    /// Move every identity under directory `from` to directory `to`.
    pub(super) fn rename_directory(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        let moved: Vec<String> = self
            .by_path
            .keys()
            .filter(|path| path.starts_with(&prefix))
            .cloned()
            .collect();
        for path in moved {
            self.rename(&path, &format!("{to}/{}", &path[prefix.len()..]));
        }
    }
}
