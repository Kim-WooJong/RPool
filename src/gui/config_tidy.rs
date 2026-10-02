//! At GUI start: drop options removed by earlier versions from the GUI
//! settings and the pool definitions (a `.bak` copy of each changed file
//! stays next to it). Files that do not parse are left for the normal
//! loading to report.
use crate::utils::prune_unknown_keys;

/// Prune unknown keys from `gui.json` and `pools.json`, reporting removals on
/// stderr. Called first by `app::launch`.
pub(crate) fn run() {
    if let Ok(path) = crate::config::gui_settings_path() {
        report(
            "GUI settings",
            prune_unknown_keys::<crate::gui::settings::GuiSettings>(&path),
        );
    }
    if let Ok(path) = crate::config::pools_path() {
        report(
            "pool definitions",
            prune_unknown_keys::<crate::models::PoolStore>(&path),
        );
    }
}

/// Print what was removed (or why tidying failed) for one file.
fn report(what: &str, outcome: anyhow::Result<Vec<String>>) {
    match outcome {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => eprintln!(
            "Removed options no longer used from the {what}: {} (backup: .bak)",
            removed.join(", ")
        ),
        Err(error) => eprintln!("Could not tidy the {what}: {error:#}"),
    }
}
