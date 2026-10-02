//! State of Maintenance › Integrity (`IntegrityForm`), including the last
//! integrity snapshot saved by a scrub (`crate::maintenance`).

use crate::maintenance::load_integrity_snapshot;
use crate::models::IntegritySnapshot;
use std::collections::BTreeSet;

/// Inputs and results of the Integrity tab, in `GuiState::integrity`.
/// Starts from the saved snapshot and its manifest.
#[derive(Debug)]
pub(crate) struct IntegrityForm {
    /// Manifest of the archive to check (local file or remote path).
    pub(crate) manifest: String,
    /// Archive ID picked from the inventory (`""` = manual manifest path).
    pub(crate) library_archive_id: String,
    /// Quick scrub (existence and size) instead of full BLAKE3; also passed to repair.
    pub(crate) quick: bool,
    /// Repair without writing (`--dry-run`).
    pub(crate) repair_dry_run: bool,
    /// Group numbers ticked for repair.
    pub(crate) selected_groups: BTreeSet<u32>,
    /// Last saved integrity snapshot, if any.
    pub(crate) snapshot: Option<IntegritySnapshot>,
    /// Load or start error.
    pub(crate) error: Option<String>,
    /// Result message of the last scrub or repair (set on task completion).
    pub(crate) notice: Option<String>,
}

impl Default for IntegrityForm {
    fn default() -> Self {
        let snapshot = load_integrity_snapshot().ok().flatten();
        let manifest = snapshot
            .as_ref()
            .map(|value| value.manifest_source.clone())
            .unwrap_or_default();
        Self {
            manifest,
            library_archive_id: String::new(),
            quick: false,
            repair_dry_run: false,
            selected_groups: BTreeSet::new(),
            snapshot,
            error: None,
            notice: None,
        }
    }
}

impl IntegrityForm {
    /// Reloads the saved snapshot after a scrub or repair and drops selected
    /// groups that are no longer recoverable. Called by
    /// `maintenance::handle_task_completion`.
    pub(crate) fn refresh_snapshot(&mut self) {
        match load_integrity_snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.error = None;
                let recoverable: std::collections::BTreeSet<u32> = self
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .groups
                            .iter()
                            .filter(|item| item.is_recoverable())
                            .map(|item| item.group)
                            .collect()
                    })
                    .unwrap_or_default();
                self.selected_groups
                    .retain(|group| recoverable.contains(group));
            }
            Err(error) => {
                self.error = Some(crate::gui::i18n::trf(
                    "Integrity snapshot could not be loaded: {error}",
                    &[("error", &format!("{error:#}"))],
                ));
            }
        }
    }
}
