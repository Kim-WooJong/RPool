//! FUSE inode numbers for paths (files and directories). Root is inode 1.
//! Renames move numbers; unlink forgets a path so a recreated file gets a new one.
use crate::prelude::*;

pub(super) struct Inodes {
    next: u64,
    by_path: BTreeMap<String, u64>,
    paths: BTreeMap<u64, String>,
}
impl Inodes {
    pub(super) fn new() -> Self {
        let mut inodes = Self {
            next: 1,
            by_path: BTreeMap::new(),
            paths: BTreeMap::new(),
        };
        inodes.by_path.insert(String::new(), 1);
        inodes.paths.insert(1, String::new());
        inodes
    }
    pub(super) fn ino(&mut self, path: &str) -> u64 {
        if let Some(ino) = self.by_path.get(path) {
            return *ino;
        }
        self.next += 1;
        self.by_path.insert(path.into(), self.next);
        self.paths.insert(self.next, path.into());
        self.next
    }
    pub(super) fn path(&self, ino: u64) -> Option<String> {
        self.paths.get(&ino).cloned()
    }
    pub(super) fn forget_path(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let gone: Vec<String> = self
            .by_path
            .keys()
            .filter(|p| p.as_str() == path || p.starts_with(&prefix))
            .cloned()
            .collect();
        for p in gone {
            if let Some(ino) = self.by_path.remove(&p) {
                self.paths.remove(&ino);
            }
        }
    }
    /// Move `from` (and anything below it) to `to`, replacing `to`.
    pub(super) fn rename(&mut self, from: &str, to: &str) {
        self.forget_path(to);
        let prefix = format!("{from}/");
        let moved: Vec<String> = self
            .by_path
            .keys()
            .filter(|p| p.as_str() == from || p.starts_with(&prefix))
            .cloned()
            .collect();
        for old in moved {
            let new = format!("{to}{}", &old[from.len()..]);
            if let Some(ino) = self.by_path.remove(&old) {
                self.by_path.insert(new.clone(), ino);
                self.paths.insert(ino, new);
            }
        }
    }
}
