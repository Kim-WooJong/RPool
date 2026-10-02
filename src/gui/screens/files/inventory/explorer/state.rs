//! Explorer state of one pool's drive and how actions change it.

use super::action::Action;
use super::nav::{parent, Nav};
use super::sort::Sort;
use crate::gui::screens::files::inventory::drive_state::DriveTree;

/// How the explorer shows a folder's entries; chosen by the List / Icons toggle in `drive.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ViewMode {
    /// Rows with Name / Size / Type columns (`list::show`).
    #[default]
    List,
    /// Grid of large tiles (`icons::show`).
    Icons,
}

/// Last scroll position of the list, to keep a moved selection in view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct ScrollMemo {
    /// Vertical scroll offset of the list, in points.
    pub(crate) offset: f32,
    /// Visible height of the scroll viewport, in points.
    pub(crate) view: f32,
}

/// Per-pool explorer state, kept in the drive form across frames and reloads.
/// Updated by `ExplorerState::apply`; read by the list/icons views.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExplorerState {
    /// Current folder and Back / Forward history.
    pub(crate) nav: Nav,
    /// Column sort order of the entries.
    pub(crate) sort: Sort,
    /// List or Icons view.
    pub(crate) view: ViewMode,
    /// Path of the selected entry.
    pub(crate) selected: Option<String>,
    /// Search the whole drive instead of the current folder.
    pub(crate) search_all: bool,
    /// Scroll position recorded by the last drawn list or grid.
    pub(crate) scroll: ScrollMemo,
    /// Scroll the selection into view on the next frame.
    pub(crate) reveal: bool,
}

impl ExplorerState {
    /// Keeps the folder and selection valid for a (re)loaded listing.
    pub(crate) fn sync(&mut self, tree: &DriveTree) {
        self.nav.retain(|path| tree.folder(path).is_some());
        if self
            .selected
            .as_deref()
            .is_some_and(|path| tree.find(path).is_none())
        {
            self.selected = None;
        }
    }

    /// Applies `action`; `rows` are the entries shown, in display order.
    pub(crate) fn apply(
        &mut self,
        action: Action,
        tree: &DriveTree,
        rows: &[usize],
        query: &mut String,
    ) {
        match action {
            Action::Select(path) => self.selected = Some(path),
            Action::Open(path) => self.open(tree, &path, query),
            Action::OpenSelected => {
                if let Some(path) = self.selected.clone() {
                    self.open(tree, &path, query);
                }
            }
            Action::Goto(path) => {
                if tree.folder(&path).is_some() {
                    self.nav.open(&path);
                    self.entered(None, query);
                }
            }
            Action::Reveal(path) => {
                self.nav.open(parent(&path));
                self.entered(Some(path), query);
            }
            Action::Back => {
                if self.nav.back() {
                    self.entered(None, query);
                }
            }
            Action::Forward => {
                if self.nav.forward() {
                    self.entered(None, query);
                }
            }
            Action::Up => {
                if let Some(left) = self.nav.up() {
                    self.entered(Some(left), query);
                }
            }
            Action::Sort(key) => self.sort.toggle(key),
            Action::Move(step) => self.step(tree, rows, step),
            // Taken by `explorer::show` before it applies an action.
            Action::History(_) => {}
        }
    }

    /// Opens `path` if it is a folder of `tree` (files are ignored).
    fn open(&mut self, tree: &DriveTree, path: &str, query: &mut String) {
        if tree.folder(path).is_some_and(|folder| folder.is_some()) {
            self.nav.open(path);
            self.entered(None, query);
        }
    }

    /// A new folder is shown: clear the search and select `selected`.
    fn entered(&mut self, selected: Option<String>, query: &mut String) {
        query.clear();
        self.reveal = selected.is_some();
        self.selected = selected;
    }

    /// Moves the selection by `step` rows in display order, clamped to the list;
    /// with nothing selected, Up selects the last row and Down the first.
    fn step(&mut self, tree: &DriveTree, rows: &[usize], step: isize) {
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|path| rows.iter().position(|&id| tree.nodes[id].path == path));
        let last = rows.len() as isize - 1;
        let next = match current {
            Some(index) => (index as isize + step).clamp(0, last),
            None if step < 0 => last,
            None => 0,
        };
        self.selected = Some(tree.nodes[rows[next as usize]].path.clone());
        self.reveal = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::screens::files::inventory::explorer::sample;

    #[test]
    fn open_back_up_and_reveal_update_folder_selection_and_search() {
        let tree = sample::tree();
        let mut state = ExplorerState::default();
        let mut query = "x".to_string();
        state.apply(Action::Open("README".into()), &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "", "files do not open");
        assert_eq!(query, "x");
        state.apply(Action::Open("Photos".into()), &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "Photos");
        assert!(query.is_empty());
        state.apply(Action::Select("Photos/2024".into()), &tree, &[], &mut query);
        state.apply(Action::OpenSelected, &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "Photos/2024");
        assert_eq!(state.selected, None);
        state.apply(Action::Up, &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "Photos");
        assert_eq!(state.selected.as_deref(), Some("Photos/2024"));
        state.apply(Action::Back, &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "Photos/2024");
        state.apply(Action::Goto(String::new()), &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "");
        state.apply(Action::Goto("missing".into()), &tree, &[], &mut query);
        assert_eq!(state.nav.current(), "");
        state.apply(
            Action::Reveal("Photos/2024/trip/beach.png".into()),
            &tree,
            &[],
            &mut query,
        );
        assert_eq!(state.nav.current(), "Photos/2024/trip");
        assert_eq!(
            state.selected.as_deref(),
            Some("Photos/2024/trip/beach.png")
        );
        assert!(state.reveal);
    }

    #[test]
    fn arrow_keys_move_the_selection_within_the_rows() {
        let tree = sample::tree();
        let rows = tree.children(None).to_vec();
        let mut state = ExplorerState::default();
        let mut query = String::new();
        state.apply(Action::Move(1), &tree, &rows, &mut query);
        assert_eq!(
            state.selected.as_deref(),
            Some(tree.nodes[rows[0]].path.as_str())
        );
        state.apply(Action::Move(-1), &tree, &rows, &mut query);
        assert_eq!(
            state.selected.as_deref(),
            Some(tree.nodes[rows[0]].path.as_str())
        );
        state.apply(Action::Move(100), &tree, &rows, &mut query);
        let last = tree.nodes[*rows.last().unwrap()].path.as_str();
        assert_eq!(state.selected.as_deref(), Some(last));
    }

    #[test]
    fn sync_after_refresh_drops_vanished_paths() {
        let tree = sample::tree();
        let mut state = ExplorerState::default();
        let mut query = String::new();
        state.apply(Action::Open("Photos".into()), &tree, &[], &mut query);
        state.apply(Action::Open("Photos/2024".into()), &tree, &[], &mut query);
        state.selected = Some("Photos/2024/IMG_0001.jpg".into());
        let smaller = crate::gui::screens::files::inventory::drive_state::DriveTree::build(
            crate::pool::browse::PoolBrowse {
                mode: "v6".into(),
                entries: vec![crate::pool::browse::BrowseEntry {
                    path: "Photos/cover.png".into(),
                    size: 1,
                    is_dir: false,
                }],
                ..Default::default()
            },
        );
        state.sync(&smaller);
        assert_eq!(state.nav.current(), "Photos");
        assert_eq!(state.selected, None);
    }
}
