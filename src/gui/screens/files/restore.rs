//! Files › Restore: download and rebuild one archive from its manifest with
//! `rpool get`, run as a background task.

use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{local_file_field, output_file_field};
use eframe::egui;
use std::ffi::OsString;

/// Inputs of the Restore form, in `GuiState::restore` (also prefilled by the
/// Library's Restore action).
#[derive(Debug, Default)]
pub(crate) struct RestoreForm {
    /// Manifest: a local file or a remote manifest path.
    pub(crate) manifest: String,
    /// Local output file to write the restored data to.
    pub(crate) output: String,
    /// Validation or start error shown above the button.
    pub(crate) error: Option<String>,
}

/// Draws the Restore form and starts the restore on click. Called by `files::show`.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading(tr("Restore"));
    ui.label(tr("Restore from a local manifest or type a remote manifest path directly. Reed-Solomon recovery is automatic when possible."));
    ui.separator();

    local_file_field(
        ui,
        tr("Manifest (local file or remote path)"),
        &mut state.restore.manifest,
        Some("rpool manifest"),
        &["json"],
    );
    output_file_field(ui, tr("Output file"), &mut state.restore.output);

    if let Some(error) = &state.restore.error {
        ui.label(error);
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(!task.is_running(), egui::Button::new(tr("Start restore")))
        .clicked()
    {
        let error = start_restore(state, task).err();
        state.restore.error = error;
    }
}

/// Validates the form and starts `rpool get <manifest> <output> --workers N
/// --retries N` with the settings' worker and retry counts.
fn start_restore(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if state.restore.manifest.trim().is_empty() {
        return Err(tr("Select or enter a manifest first.").to_string());
    }
    if state.restore.output.trim().is_empty() {
        return Err(tr("Choose an output file first.").to_string());
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
