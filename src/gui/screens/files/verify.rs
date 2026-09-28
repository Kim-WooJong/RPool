use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::local_file_field;
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug, Default)]
pub(crate) struct VerifyForm {
    pub(crate) manifest: String,
    pub(crate) full: bool,
    pub(crate) error: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Verify");
    ui.label("Check every physical shard. Full verification streams each shard and validates its BLAKE3 hash.");
    ui.separator();

    local_file_field(
        ui,
        "Manifest (local file or remote path)",
        &mut state.verify.manifest,
        Some("rpool manifest"),
        &["json"],
    );
    ui.checkbox(
        &mut state.verify.full,
        "Full remote read + BLAKE3 verification",
    );

    if let Some(error) = &state.verify.error {
        ui.label(error);
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(!task.is_running(), egui::Button::new("Start verification"))
        .clicked()
    {
        let error = start_verify(state, task).err();
        state.verify.error = error;
    }
}

fn start_verify(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if state.verify.manifest.trim().is_empty() {
        return Err("Select or enter a manifest first.".to_string());
    }

    let mut args = vec![
        OsString::from("verify"),
        OsString::from(state.verify.manifest.trim()),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
    ];
    if state.verify.full {
        args.push(OsString::from("--full"));
    }
    task.start_rpool("Verify", &state.settings.rclone, args)
}
