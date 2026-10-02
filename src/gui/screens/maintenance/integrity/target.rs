//! Target picker of Maintenance › Integrity: an archive from the local
//! inventory or a manifest path typed or browsed.

use super::state::IntegrityForm;
use crate::gui::i18n::tr;
use crate::gui::widgets::local_file_field;
use crate::inventory::load_inventory;
use eframe::egui;

/// Draws the inventory combo box (choosing an entry fills `form.manifest`) and
/// the manifest field. Called by `integrity::show`.
pub(crate) fn show(ui: &mut egui::Ui, form: &mut IntegrityForm) {
    let entries = load_inventory()
        .map(|store| {
            let mut values: Vec<_> = store.entries.into_values().collect();
            values.sort_by(|a, b| {
                a.original_name
                    .to_ascii_lowercase()
                    .cmp(&b.original_name.to_ascii_lowercase())
            });
            values
        })
        .unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label(tr("Library"));
        egui::ComboBox::from_id_salt("integrity-library-target")
            .selected_text(if form.library_archive_id.is_empty() {
                tr("Choose indexed archive…").to_string()
            } else {
                entries
                    .iter()
                    .find(|entry| entry.archive_id == form.library_archive_id)
                    .map(|entry| entry.original_name.clone())
                    .unwrap_or_else(|| tr("Choose indexed archive…").to_string())
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut form.library_archive_id,
                    String::new(),
                    tr("Manual manifest path"),
                );
                for entry in &entries {
                    if ui
                        .selectable_value(
                            &mut form.library_archive_id,
                            entry.archive_id.clone(),
                            format!(
                                "{}  ({})",
                                entry.original_name,
                                &entry.archive_id[..entry.archive_id.len().min(10)]
                            ),
                        )
                        .clicked()
                    {
                        form.manifest = entry.manifest_source.clone();
                    }
                }
            });
    });

    local_file_field(
        ui,
        tr("Manifest"),
        &mut form.manifest,
        Some("rpool manifest"),
        &["json"],
    );
}
