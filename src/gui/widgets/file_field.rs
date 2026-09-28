use eframe::egui;

pub(crate) fn local_file_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    filter_name: Option<&str>,
    extensions: &[&str],
) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        if ui.button("Browse…").clicked() {
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

pub(crate) fn output_file_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        if ui.button("Browse…").clicked() {
            if let Some(path) = rfd::FileDialog::new().save_file() {
                *value = path.display().to_string();
            }
        }
    });
}
