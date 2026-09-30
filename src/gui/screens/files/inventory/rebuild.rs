use super::state::InventoryForm;
use crate::gui::i18n::tr;
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(
    ui: &mut egui::Ui,
    form: &mut InventoryForm,
    task: &mut TaskRunner,
    rclone: &str,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(tr("Rebuild inventory")).strong());
        ui.label(egui::RichText::new(tr("Recreate the local index from manifest files.")).weak());
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.manifest_directory)
                    .hint_text(tr("Directory containing manifests"))
                    .desired_width(320.0),
            );
            if ui.button(tr("Browse…")).clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    form.manifest_directory = path.display().to_string();
                }
            }
            if ui
                .add_enabled(!task.is_running(), egui::Button::new(tr("Rebuild")))
                .clicked()
            {
                form.error = start_rebuild(form, task, rclone).err();
            }
        });
        ui.label(
            egui::RichText::new(tr(
                "Refresh the file list after the rebuild task completes.",
            ))
            .small()
            .weak(),
        );
    });
}

fn start_rebuild(form: &InventoryForm, task: &mut TaskRunner, rclone: &str) -> Result<(), String> {
    let directory = form.manifest_directory.trim();
    if directory.is_empty() {
        return Err(tr("Choose a manifest directory first.").to_string());
    }
    task.start_rpool(
        "Inventory rebuild",
        rclone,
        [
            OsString::from("inventory"),
            OsString::from("rebuild"),
            OsString::from(directory),
        ],
    )
}
