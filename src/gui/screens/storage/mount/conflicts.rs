//! Pool-sync conflicts from the last synchronized metadata.
use super::form::MountForm;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, form: &MountForm) {
    let Some(status) = &form.pool_status else {
        return;
    };
    let title = if status.conflicts.is_empty() {
        "Pool sync · no conflicts".to_string()
    } else {
        format!("Pool sync · {} conflict groups", status.conflicts.len())
    };
    egui::CollapsingHeader::new(title)
        .id_salt("mount-conflicts")
        .default_open(!status.conflicts.is_empty())
        .show(ui, |ui| {
            if !form.runner.is_running() {
                ui.small("Last sync snapshot, not a live cloud view.");
            }
            ui.small(format!(
                "{} metadata destinations · previous-version limit {} · history deletion {}",
                status.roots.len(),
                status.desired_history_limit,
                if status.history_deletion_enabled {
                    "on"
                } else {
                    "off"
                }
            ));
            for conflict in &status.conflicts {
                ui.collapsing(&conflict.path, |ui| {
                    for original in &conflict.originals {
                        ui.horizontal(|ui| {
                            ui.label(format!("Original: {original}"));
                            if ui.small_button("Copy path").clicked() {
                                ui.ctx().copy_text(original.clone());
                            }
                        });
                    }
                    if conflict.original_unavailable {
                        ui.label("No common original (for example, created concurrently).");
                    }
                    if conflict.ambiguous_original {
                        ui.label("Several common originals — review manually.");
                    }
                    for branch in &conflict.branches {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{} · {}",
                                branch.worker,
                                branch.file.as_deref().unwrap_or("Deletion request")
                            ));
                            if let Some(path) = &branch.file {
                                if ui.small_button("Copy path").clicked() {
                                    ui.ctx().copy_text(path.clone());
                                }
                            }
                        });
                    }
                    ui.small(
                        "All variants are kept; nothing is merged or discarded automatically.",
                    );
                });
            }
        });
}
