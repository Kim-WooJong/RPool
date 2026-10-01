//! Explorer navigation: the current folder and Back / Forward history.
//! Paths are normalized drive paths; "" is the drive root.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Nav {
    current: String,
    back: Vec<String>,
    forward: Vec<String>,
}

impl Nav {
    pub(crate) fn current(&self) -> &str {
        &self.current
    }

    pub(crate) fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub(crate) fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub(crate) fn can_up(&self) -> bool {
        !self.current.is_empty()
    }

    /// Opens `path` (a folder or a breadcrumb segment): the old folder goes
    /// to Back and Forward is cleared, as in a file manager.
    pub(crate) fn open(&mut self, path: &str) {
        if path == self.current {
            return;
        }
        self.back
            .push(std::mem::replace(&mut self.current, path.to_string()));
        self.forward.clear();
    }

    pub(crate) fn back(&mut self) -> bool {
        let Some(previous) = self.back.pop() else {
            return false;
        };
        self.forward
            .push(std::mem::replace(&mut self.current, previous));
        true
    }

    pub(crate) fn forward(&mut self) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        self.back.push(std::mem::replace(&mut self.current, next));
        true
    }

    /// Opens the parent folder; returns the folder that was left.
    pub(crate) fn up(&mut self) -> Option<String> {
        if self.current.is_empty() {
            return None;
        }
        let left = self.current.clone();
        self.open(parent(&left));
        Some(left)
    }

    /// After a refresh: walks up from folders that no longer exist and drops
    /// vanished folders from the history.
    pub(crate) fn retain(&mut self, exists: impl Fn(&str) -> bool) {
        while !self.current.is_empty() && !exists(&self.current) {
            self.current = parent(&self.current).to_string();
        }
        // The root always exists.
        self.back.retain(|path| path.is_empty() || exists(path));
        self.forward.retain(|path| path.is_empty() || exists(path));
    }
}

pub(crate) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_back_forward_and_up() {
        let mut nav = Nav::default();
        assert!(!nav.can_back() && !nav.can_forward() && !nav.can_up());
        nav.open("Photos");
        nav.open("Photos/2024");
        assert_eq!(nav.current(), "Photos/2024");
        assert!(nav.back());
        assert_eq!(nav.current(), "Photos");
        assert!(nav.can_forward());
        assert!(nav.forward());
        assert_eq!(nav.current(), "Photos/2024");
        assert!(!nav.forward());
        assert_eq!(nav.up().as_deref(), Some("Photos/2024"));
        assert_eq!(nav.current(), "Photos");
        assert_eq!(nav.up().as_deref(), Some("Photos"));
        assert_eq!(nav.current(), "");
        assert_eq!(nav.up(), None);
        // Up is history too: Back returns to the folder that was left.
        assert!(nav.back());
        assert_eq!(nav.current(), "Photos");
    }

    #[test]
    fn opening_clears_forward_and_same_folder_is_a_no_op() {
        let mut nav = Nav::default();
        nav.open("a");
        nav.open("a/b");
        nav.back();
        nav.open("a");
        assert!(
            nav.can_forward(),
            "re-opening the current folder keeps Forward"
        );
        nav.open("c");
        assert!(!nav.can_forward());
        assert!(nav.back());
        assert_eq!(nav.current(), "a");
    }

    #[test]
    fn breadcrumb_jump_is_an_open() {
        let mut nav = Nav::default();
        nav.open("a/b/c");
        nav.open("a");
        assert_eq!(nav.current(), "a");
        assert!(nav.back());
        assert_eq!(nav.current(), "a/b/c");
    }

    #[test]
    fn retain_walks_up_from_removed_folders() {
        let mut nav = Nav::default();
        nav.open("gone");
        nav.open("a/b/c");
        nav.retain(|path| path == "a");
        assert_eq!(nav.current(), "a");
        assert!(nav.can_back(), "the root stays in Back");
        nav.back();
        assert_eq!(nav.current(), "");
        assert!(!nav.can_back());
    }
}
