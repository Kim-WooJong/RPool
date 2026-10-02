//! What the user asked the explorer to do this frame; applied after drawing.

use super::sort::SortKey;

/// One user request produced while drawing the explorer (clicks, keys, menus).
/// Collected by `explorer::show` and applied afterwards by `ExplorerState::apply`;
/// `History` requests are passed up to the history views instead.
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
    /// Back button / Alt+Left: return to the previous folder.
    Back,
    /// Forward button / Alt+Right: redo a Back.
    Forward,
    /// Up button / Backspace: open the parent folder.
    Up,
    /// Column header click: sort by this key (toggles direction on the active key).
    Sort(SortKey),
    /// Arrow keys: move the selection by this many rows.
    Move(isize),
    /// Versions, rollback: handled by the history views, not the explorer.
    History(HistoryRequest),
}

/// What the explorer asks the trash/versions/rollback views to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryRequest {
    /// The versions panel of a file (explorer path).
    Versions(String),
    /// The rollback dialog of a folder (explorer path, `""` = whole drive).
    Rollback(String),
}
