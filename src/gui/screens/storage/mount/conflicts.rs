//! Pool-sync conflicts from the last synchronized metadata.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use eframe::egui;

/// Collapsible list of pool-sync conflicts from the session's last
/// `pool-sync-status.json`; hidden when no status was read.
pub(super) fn show(ui: &mut egui::Ui, form: &MountForm) {
    let Some(status) = &form.session.pool_status else {
        return;
    };
    let title = if status.conflicts.is_empty() {
        tr("Pool sync · no conflicts").to_string()
    } else {
        trf(
            "Pool sync · {n} conflict groups",
            &[("n", &status.conflicts.len())],
        )
    };
    egui::CollapsingHeader::new(title)
        .id_salt("mount-conflicts")
        .default_open(!status.conflicts.is_empty())
        .show(ui, |ui| {
            if !form.session.runner.is_running() {
                ui.small(tr("Last sync snapshot, not a live cloud view."));
            }
            ui.small(trf(
                "{roots} metadata destinations",
                &[("roots", &status.roots.len())],
            ));
            for conflict in &status.conflicts {
                ui.collapsing(&conflict.path, |ui| {
                    for original in &conflict.originals {
                        ui.horizontal(|ui| {
                            ui.label(trf("Original: {path}", &[("path", original)]));
                            if ui.small_button(tr("Copy path")).clicked() {
                                ui.ctx().copy_text(original.clone());
                            }
                        });
                    }
                    if conflict.original_unavailable {
                        ui.label(tr(
                            "No common original (for example, created concurrently).",
                        ));
                    }
                    if conflict.ambiguous_original {
                        ui.label(tr("Several common originals — review manually."));
                    }
                    for branch in &conflict.branches {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{} · {}",
                                branch.worker,
                                branch.file.as_deref().unwrap_or(tr("Deletion request"))
                            ));
                            if let Some(path) = &branch.file {
                                if ui.small_button(tr("Copy path")).clicked() {
                                    ui.ctx().copy_text(path.clone());
                                }
                            }
                        });
                    }
                    ui.small(tr(
                        "All variants are kept; nothing is merged or discarded automatically.",
                    ));
                });
            }
        });
}
