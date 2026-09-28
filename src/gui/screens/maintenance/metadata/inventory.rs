use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.label(egui::RichText::new("Inventory cache").strong());
    ui.label(egui::RichText::new("Rebuild the local inventory index from manifest files. The index is not a source of truth.").weak());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.manifest.inventory_directory)
                .hint_text("Directory containing manifests")
                .desired_width(360.0),
        );
        if ui.button("Browse…").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.manifest.inventory_directory = path.display().to_string();
            }
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Rebuild inventory"))
            .clicked()
        {
            state.manifest.error = start(state, task).err();
            state.manifest.notice = None;
        }
    });
}

fn start(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let directory = state.manifest.inventory_directory.trim();
    if directory.is_empty() {
        return Err("Choose a directory containing manifests first.".to_string());
    }
    task.start_rpool(
        "Inventory rebuild",
        &state.settings.rclone,
        [
            OsString::from("inventory"),
            OsString::from("rebuild"),
            OsString::from(directory),
        ],
    )
}
