use super::state::IntegrityForm;
use crate::inventory::load_inventory;
use crate::gui::widgets::local_file_field;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, form: &mut IntegrityForm) {
    let entries = load_inventory()
        .map(|store| {
            let mut values: Vec<_> = store.entries.into_values().collect();
            values.sort_by(|a, b| a.original_name.to_ascii_lowercase().cmp(&b.original_name.to_ascii_lowercase()));
            values
        })
        .unwrap_or_default();

    ui.horizontal(|ui| {
        ui.label("Library");
        egui::ComboBox::from_id_salt("integrity-library-target")
            .selected_text(if form.library_archive_id.is_empty() {
                "Choose indexed archive…".to_string()
            } else {
                entries
                    .iter()
                    .find(|entry| entry.archive_id == form.library_archive_id)
                    .map(|entry| entry.original_name.clone())
                    .unwrap_or_else(|| "Choose indexed archive…".to_string())
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut form.library_archive_id, String::new(), "Manual manifest path");
                for entry in &entries {
                    if ui
                        .selectable_value(
                            &mut form.library_archive_id,
                            entry.archive_id.clone(),
                            format!("{}  ({})", entry.original_name, &entry.archive_id[..entry.archive_id.len().min(10)]),
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
        "Manifest",
        &mut form.manifest,
        Some("rpool manifest"),
        &["json"],
    );
}
