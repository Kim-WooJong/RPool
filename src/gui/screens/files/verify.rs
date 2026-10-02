//! Archive verify form: runs `rpool verify` on a manifest, optionally reading
//! every shard back and checking its BLAKE3 hash (`--full`).

use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::local_file_field;
use eframe::egui;
use std::ffi::OsString;

/// Inputs of the verify form, in `GuiState::verify` (also prefilled by the
/// Library's Verify action).
#[derive(Debug, Default)]
pub(crate) struct VerifyForm {
    /// Manifest: a local file or a remote manifest path.
    pub(crate) manifest: String,
    /// Full verification: stream each shard and check its BLAKE3 hash.
    pub(crate) full: bool,
    /// Validation or start error shown above the button.
    pub(crate) error: Option<String>,
}

/// Draws the verify form and starts verification on click. Called by
/// Maintenance › Archive.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading(tr("Verify"));
    ui.label(tr("Check every physical shard. Full verification streams each shard and validates its BLAKE3 hash."));
    ui.separator();

    local_file_field(
        ui,
        tr("Manifest (local file or remote path)"),
        &mut state.verify.manifest,
        Some("rpool manifest"),
        &["json"],
    );
    ui.checkbox(
        &mut state.verify.full,
        tr("Full remote read + BLAKE3 verification"),
    );

    if let Some(error) = &state.verify.error {
        ui.label(error);
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(
            !task.is_running(),
            egui::Button::new(tr("Start verification")),
        )
        .clicked()
    {
        let error = start_verify(state, task).err();
        state.verify.error = error;
    }
}

/// Validates the manifest and starts `rpool verify <manifest> --workers N
/// [--full]` as a background task.
fn start_verify(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if state.verify.manifest.trim().is_empty() {
        return Err(tr("Select or enter a manifest first.").to_string());
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
