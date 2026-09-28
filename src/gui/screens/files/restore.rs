use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{local_file_field, output_file_field};
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug, Default)]
pub(crate) struct RestoreForm {
    pub(crate) manifest: String,
    pub(crate) output: String,
    pub(crate) error: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Restore");
    ui.label("Restore from a local manifest or type a remote manifest path directly. Reed-Solomon recovery is automatic when possible.");
    ui.separator();

    local_file_field(
        ui,
        "Manifest (local file or remote path)",
        &mut state.restore.manifest,
        Some("rpool manifest"),
        &["json"],
    );
    output_file_field(ui, "Output file", &mut state.restore.output);

    if let Some(error) = &state.restore.error {
        ui.label(error);
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(!task.is_running(), egui::Button::new("Start restore"))
        .clicked()
    {
        let error = start_restore(state, task).err();
        state.restore.error = error;
    }
}

fn start_restore(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if state.restore.manifest.trim().is_empty() {
        return Err("Select or enter a manifest first.".to_string());
    }
    if state.restore.output.trim().is_empty() {
        return Err("Choose an output file first.".to_string());
    }

    let args = vec![
        OsString::from("get"),
        OsString::from(state.restore.manifest.trim()),
        OsString::from(state.restore.output.trim()),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
        OsString::from("--retries"),
        OsString::from(state.settings.retries.to_string()),
    ];
    task.start_rpool("Restore", &state.settings.rclone, args)
}
