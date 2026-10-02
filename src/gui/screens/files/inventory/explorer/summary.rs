//! Counts, sizes and type labels shown in the list and the details line.

use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use crate::presentation::format_bytes;

/// Direct contents of one folder; `bytes` counts every file below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FolderSummary {
    /// Number of direct subfolders.
    pub(crate) folders: usize,
    /// Number of direct files.
    pub(crate) files: usize,
    /// Total bytes of all files below the folder.
    pub(crate) bytes: u64,
}

impl FolderSummary {
    /// Summary of `folder` (`None` = drive root) of `tree`. Used by `explorer::show`
    /// for the footer line.
    pub(crate) fn of(tree: &DriveTree, folder: Option<usize>) -> Self {
        let children = tree.children(folder);
        let folders = children.iter().filter(|&&id| tree.nodes[id].is_dir).count();
        Self {
            folders,
            files: children.len() - folders,
            bytes: folder.map_or(tree.bytes, |id| tree.nodes[id].size),
        }
    }

    /// "12 folders, 340 files, 5.2 GiB".
    pub(crate) fn label(&self) -> String {
        format!(
            "{}, {}, {}",
            folders_label(self.folders),
            files_label(self.files),
            format_bytes(self.bytes)
        )
    }
}

/// Lowercased extension of a file ("" for folders and names without one;
/// a leading dot alone, as in ".env", is not an extension).
pub(crate) fn extension(name: &str, is_dir: bool) -> String {
    if is_dir {
        return String::new();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_lowercase(),
        _ => String::new(),
    }
}

/// The Type column: "Folder", the extension in capitals, or "File".
pub(crate) fn type_label(name: &str, is_dir: bool) -> String {
    if is_dir {
        return tr("Folder").to_string();
    }
    match extension(name, false) {
        ext if ext.is_empty() => tr("File").to_string(),
        ext => ext.to_uppercase(),
    }
}

/// Folder or file glyph shown before names in the list, grid and details line.
pub(crate) fn icon(is_dir: bool) -> &'static str {
    if is_dir {
        "📁"
    } else {
        "📄"
    }
}

/// "{n} folder(s)", translated and pluralized.
pub(crate) fn folders_label(n: usize) -> String {
    if n == 1 {
        trf("{n} folder", &[("n", &n)])
    } else {
        trf("{n} folders", &[("n", &n)])
    }
}

/// "{n} file(s)", translated and pluralized.
pub(crate) fn files_label(n: usize) -> String {
    if n == 1 {
        trf("{n} file", &[("n", &n)])
    } else {
        trf("{n} files", &[("n", &n)])
    }
}

/// "{n} item(s)": a folder's direct entry count in the Size column and details.
pub(crate) fn items_label(n: usize) -> String {
    if n == 1 {
        trf("{n} item", &[("n", &n)])
    } else {
        trf("{n} items", &[("n", &n)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::screens::files::inventory::explorer::sample;

    #[test]
    fn folder_counts_and_sizes() {
        let tree = sample::tree();
        let root = FolderSummary::of(&tree, None);
        assert_eq!((root.folders, root.files), (3, 3));
        assert_eq!(root.bytes, tree.bytes);
        let photos = tree.folder("Photos").unwrap();
        let summary = FolderSummary::of(&tree, photos);
        assert_eq!((summary.folders, summary.files), (1, 1));
        assert_eq!(
            summary.bytes,
            3_000_000 + 2_900_000 + 4_000_000 + 500_000 + 3_000_000
        );
        let burst = tree.folder("Photos/2024/burst").unwrap();
        assert_eq!(FolderSummary::of(&tree, burst).files, 30);
        assert_eq!(
            FolderSummary {
                folders: 12,
                files: 1,
                bytes: 0
            }
            .label(),
            "12 folders, 1 file, 0 B"
        );
    }

    #[test]
    fn types_come_from_extensions() {
        assert_eq!(extension("a.TAR.Zst", false), "zst");
        assert_eq!(extension(".env", false), "");
        assert_eq!(extension("README", false), "");
        assert_eq!(extension("dir.d", true), "");
        assert_eq!(type_label("photo.jpg", false), "JPG");
        assert_eq!(type_label("README", false), "File");
        assert_eq!(type_label("x.y", true), "Folder");
    }
}
