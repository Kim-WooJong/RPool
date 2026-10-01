//! What the user asked the explorer to do this frame; applied after drawing.

use super::sort::SortKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Single click: select an entry by path.
    Select(String),
    /// Double click / Enter on a folder.
    Open(String),
    /// Enter: open the selected entry if it is a folder.
    OpenSelected,
    /// Breadcrumb segment.
    Goto(String),
    /// A search result's folder: open it and select the result.
    Reveal(String),
    Back,
    Forward,
    Up,
    Sort(SortKey),
    /// Arrow keys: move the selection by this many rows.
    Move(isize),
}
