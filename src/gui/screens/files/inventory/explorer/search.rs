//! The entries the list shows: the current folder, filtered by name, or
//! name matches anywhere on the drive ("Search all folders").

use crate::gui::screens::files::inventory::drive_state::DriveTree;

/// Unsorted entry ids. An empty query lists the folder itself.
pub(crate) fn entries(
    tree: &DriveTree,
    folder: Option<usize>,
    query: &str,
    all: bool,
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return tree.children(folder).to_vec();
    }
    let matches = |id: &usize| tree.nodes[*id].lower_name().contains(&query);
    if all {
        (0..tree.nodes.len()).filter(matches).collect()
    } else {
        tree.children(folder)
            .iter()
            .copied()
            .filter(matches)
            .collect()
    }
}

/// Whether the list shows drive-wide results (with their folder column).
pub(crate) fn is_global(query: &str, all: bool) -> bool {
    all && !query.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::screens::files::inventory::explorer::sample;

    fn paths(tree: &DriveTree, mut ids: Vec<usize>) -> Vec<String> {
        ids.sort_by(|a, b| tree.nodes[*a].path.cmp(&tree.nodes[*b].path));
        ids.into_iter()
            .map(|id| tree.nodes[id].path.clone())
            .collect()
    }

    #[test]
    fn current_folder_versus_all_folders() {
        let tree = sample::tree();
        let photos = tree.folder("Photos").unwrap();
        assert_eq!(entries(&tree, photos, "", false).len(), 2);
        assert_eq!(
            paths(&tree, entries(&tree, photos, " PNG ", false)),
            ["Photos/cover.png"]
        );
        assert_eq!(
            paths(&tree, entries(&tree, photos, "png", true)),
            ["Photos/2024/trip/beach.png", "Photos/cover.png"]
        );
        // Names only: the folder part of a path does not match.
        assert!(entries(&tree, None, "photos/", true).is_empty());
        assert!(entries(&tree, None, "IMG", false).is_empty());
        assert_eq!(entries(&tree, None, "img", true).len(), 2);
        assert!(is_global("img", true) && !is_global("  ", true) && !is_global("img", false));
    }
}
