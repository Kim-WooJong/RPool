//! Archive status form: runs `rpool status` on a manifest (shard availability,
//! degraded groups, recoverability, failure domains), optionally with provider usage.

use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::local_file_field;
use eframe::egui;
use std::ffi::OsString;

/// Inputs of the status form, in `GuiState::status` (also prefilled by the
/// Library's Status action).
#[derive(Debug)]
pub(crate) struct StatusForm {
    /// Manifest: a local file or a remote manifest path.
    pub(crate) manifest: String,
    /// Pass `--usage` to append provider usage (on by default).
    pub(crate) include_usage: bool,
    /// Validation or start error shown above the button.
    pub(crate) error: Option<String>,
}

impl Default for StatusForm {
    fn default() -> Self {
        Self {
            manifest: String::new(),
            include_usage: true,
            error: None,
        }
    }
}

/// Draws the status form and starts the check on click. Called by
/// Maintenance › Archive.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading(tr("Archive status"));
    ui.label(tr("Inspect shard availability, degraded groups, recoverability, and provider failure-domain safety."));
    ui.separator();

    local_file_field(
        ui,
        tr("Manifest (local file or remote path)"),
        &mut state.status.manifest,
        Some("rpool manifest"),
        &["json"],
    );
    ui.checkbox(
        &mut state.status.include_usage,
        tr("Append provider usage information"),
    );

    if let Some(error) = &state.status.error {
        ui.label(error);
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(!task.is_running(), egui::Button::new(tr("Check status")))
        .clicked()
    {
        let error = start_status(state, task).err();
        state.status.error = error;
    }
}

/// Validates the manifest and starts `rpool status <manifest> --workers N
/// [--usage]` as a background task.
fn start_status(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if state.status.manifest.trim().is_empty() {
        return Err(tr("Select or enter a manifest first.").to_string());
    }

    let mut args = vec![
        OsString::from("status"),
        OsString::from(state.status.manifest.trim()),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
    ];
    if state.status.include_usage {
        args.push(OsString::from("--usage"));
    }
    task.start_rpool("Status", &state.settings.rclone, args)
}
