//! Labelled path text fields with a native Browse… dialog (`rfd`).

use crate::gui::i18n::tr;
use eframe::egui;

/// Leaves room for the Browse button so the row never overflows.
fn field_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() - 96.0).clamp(120.0, 640.0)
}

/// Text field plus a Browse… button that picks an existing file; `filter_name`
/// and `extensions` add an optional file-type filter. Used by restore, status
/// and provider screens.
pub(crate) fn local_file_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    filter_name: Option<&str>,
    extensions: &[&str],
) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(field_width(ui)));
        if ui.button(tr("Browse…")).clicked() {
            let mut dialog = rfd::FileDialog::new();
            if let Some(name) = filter_name {
                dialog = dialog.add_filter(name, extensions);
            }
            if let Some(path) = dialog.pick_file() {
                *value = path.display().to_string();
            }
        }
    });
}

/// Text field plus a Browse… button that picks a save location.
pub(crate) fn output_file_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(field_width(ui)));
        if ui.button(tr("Browse…")).clicked() {
            if let Some(path) = rfd::FileDialog::new().save_file() {
                *value = path.display().to_string();
            }
        }
    });
}
