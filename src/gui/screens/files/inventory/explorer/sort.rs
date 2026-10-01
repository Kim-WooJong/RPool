//! Column sorting of the explorer list; folders always come first.

use super::summary::extension;
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SortKey {
    #[default]
    Name,
    Size,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Sort {
    pub(crate) key: SortKey,
    pub(crate) descending: bool,
}

impl Sort {
    /// Clicking the active column flips the direction; another column
    /// sorts ascending by it.
    pub(crate) fn toggle(&mut self, key: SortKey) {
        if self.key == key {
            self.descending = !self.descending;
        } else {
            *self = Self {
                key,
                descending: false,
            };
        }
    }

    /// `▲`/`▼` after the active column's label.
    pub(crate) fn arrow(&self, key: SortKey) -> &'static str {
        match (self.key == key, self.descending) {
            (false, _) => "",
            (true, false) => " ▲",
            (true, true) => " ▼",
        }
    }

    pub(crate) fn apply(&self, tree: &DriveTree, ids: &mut [usize]) {
        ids.sort_by(|&a, &b| self.compare(tree, a, b));
    }

    fn compare(&self, tree: &DriveTree, a: usize, b: usize) -> Ordering {
        let (left, right) = (&tree.nodes[a], &tree.nodes[b]);
        let by_name = || {
            left.lower_name()
                .cmp(right.lower_name())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        };
        let by_key = match self.key {
            SortKey::Name => by_name(),
            SortKey::Size => left.size.cmp(&right.size).then_with(by_name),
            SortKey::Type => extension(&left.name, left.is_dir)
                .cmp(&extension(&right.name, right.is_dir))
                .then_with(by_name),
        };
        let by_key = if self.descending {
            by_key.reverse()
        } else {
            by_key
        };
        right.is_dir.cmp(&left.is_dir).then(by_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::screens::files::inventory::explorer::sample::tree;

    fn names(tree: &DriveTree, sort: Sort) -> Vec<String> {
        let mut ids = tree.children(None).to_vec();
        sort.apply(tree, &mut ids);
        ids.iter().map(|&id| tree.nodes[id].name.clone()).collect()
    }

    #[test]
    fn folders_first_in_every_order() {
        let tree = tree();
        let mut sort = Sort::default();
        assert_eq!(
            names(&tree, sort),
            [
                "Documents",
                "Music",
                "Photos",
                "archive.tar.zst",
                "notes.txt",
                "README"
            ]
        );
        sort.toggle(SortKey::Name);
        assert!(sort.descending);
        assert_eq!(
            names(&tree, sort),
            [
                "Photos",
                "Music",
                "Documents",
                "README",
                "notes.txt",
                "archive.tar.zst"
            ]
        );
        sort.toggle(SortKey::Size);
        assert!(!sort.descending);
        let by_size = names(&tree, sort);
        assert_eq!(&by_size[3..], ["notes.txt", "README", "archive.tar.zst"]);
        assert_eq!(&by_size[..3], ["Music", "Documents", "Photos"]);
        sort.toggle(SortKey::Size);
        assert_eq!(
            names(&tree, sort),
            [
                "Photos",
                "Documents",
                "Music",
                "archive.tar.zst",
                "README",
                "notes.txt"
            ]
        );
        sort.toggle(SortKey::Type);
        // No extension sorts first, then ".txt" < ".zst"; folders tie by name.
        assert_eq!(
            names(&tree, sort),
            [
                "Documents",
                "Music",
                "Photos",
                "README",
                "notes.txt",
                "archive.tar.zst"
            ]
        );
        assert_eq!(sort.arrow(SortKey::Type), " ▲");
        assert_eq!(sort.arrow(SortKey::Name), "");
    }
}
