use crate::maintenance::load_integrity_snapshot;
use crate::models::IntegritySnapshot;
use std::collections::BTreeSet;

#[derive(Debug)]
pub(crate) struct IntegrityForm {
    pub(crate) manifest: String,
    pub(crate) library_archive_id: String,
    pub(crate) quick: bool,
    pub(crate) repair_dry_run: bool,
    pub(crate) selected_groups: BTreeSet<u32>,
    pub(crate) snapshot: Option<IntegritySnapshot>,
    pub(crate) error: Option<String>,
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
                self.error = Some(format!("Integrity snapshot could not be loaded: {error:#}"));
            }
        }
    }
}
