//! Upload target controls: the storage pool picker, or the manual one-off
//! summary and destination selector.

use super::state::UploadTargetMode;
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::widgets::remote_selector;
use eframe::egui;

/// Draws the main target control for the current mode. Called by `upload::show`.
pub(crate) fn show_primary(ui: &mut egui::Ui, state: &mut GuiState) {
    match state.upload.target_mode {
        UploadTargetMode::Pool => show_pool_selector(ui, state),
        UploadTargetMode::Manual => show_manual_summary(ui, state),
    }
}

/// Pool combo box with a hint about where destinations and redundancy come from.
fn show_pool_selector(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(egui::RichText::new(tr("Storage pool")).strong());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("upload-pool")
            .selected_text(if state.upload.pool_name.is_empty() {
                tr("Select a pool")
            } else {
                state.upload.pool_name.as_str()
            })
            .show_ui(ui, |ui| {
                for name in &state.pool_names {
                    ui.selectable_value(&mut state.upload.pool_name, name.clone(), name.as_str());
                }
            });
    });

    if state.pool_names.is_empty() {
        ui.label(
            egui::RichText::new(
                tr("No storage pool is configured. Create one under Storage > Pools, or open Advanced for a one-off manual upload."),
            )
            .weak(),
        );
    } else if !state.upload.pool_name.is_empty() {
        ui.label(
            egui::RichText::new(tr(
                "Destinations and redundancy settings come from the selected pool.",
            ))
            .weak(),
        );
    }
}

/// Manual mode: how many destinations are configured, pointing to Advanced.
fn show_manual_summary(ui: &mut egui::Ui, state: &GuiState) {
    let count = state
        .settings
        .remotes
        .iter()
        .filter(|remote| !remote.trim().is_empty())
        .count();
    ui.label(egui::RichText::new(tr("Manual one-off upload")).strong());
    ui.label(
        egui::RichText::new(trf(
            "{n} destination(s) configured. Open Advanced to review or change the manual upload policy.",
            &[("n", &count)],
        ))
        .weak(),
    );
}

/// Remote selector for manual destinations (saved settings remotes), shown in
/// Advanced.
pub(crate) fn show_manual_destinations(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(
        egui::RichText::new(
            tr("Manual destinations are intended for one-off uploads. New archive data is still restricted to safe rclone crypt remotes."),
        )
        .weak(),
    );
    remote_selector(
        ui,
        &mut state.settings.remotes,
        &state.crypt_remotes,
        &state.settings.default_remote_path,
        &state.remote_roots,
        &mut state.upload.manual_remote,
    );
}
