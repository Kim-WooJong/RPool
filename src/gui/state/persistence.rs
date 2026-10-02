//! Startup loading of the GUI state from the files on disk.

use super::app_state::GuiState;
use crate::gui::settings as gui_settings;

/// Builds the initial [`GuiState`]: GUI settings plus the saved pool definitions
/// and remote roots (a missing or unreadable store gives an empty list).
/// Called once by `gui::app` at startup.
pub(crate) fn load(startup_rclone: &str) -> GuiState {
    let settings = gui_settings::load(startup_rclone);
    let pool_definitions = crate::pool::load_pool_store()
        .map(|store| store.pools)
        .unwrap_or_default();
    let remote_roots = crate::remote_root::load_remote_root_store()
        .map(|store| store.roots)
        .unwrap_or_default();

    GuiState::new(settings, remote_roots, pool_definitions)
}
