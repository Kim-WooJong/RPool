use super::state::UploadTargetMode;
use crate::gui::state::GuiState;
use crate::gui::widgets::remote_selector;
use eframe::egui;

pub(crate) fn show_primary(ui: &mut egui::Ui, state: &mut GuiState) {
    match state.upload.target_mode {
        UploadTargetMode::Pool => show_pool_selector(ui, state),
        UploadTargetMode::Manual => show_manual_summary(ui, state),
    }
}

fn show_pool_selector(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(egui::RichText::new("Storage pool").strong());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("upload-pool")
            .selected_text(if state.upload.pool_name.is_empty() {
                "Select a pool"
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
                "No storage pool is configured. Create one under Storage > Pools, or open Advanced for a one-off manual upload.",
            )
            .weak(),
        );
    } else if !state.upload.pool_name.is_empty() {
        ui.label(
            egui::RichText::new(
                "Destinations and redundancy settings come from the selected pool.",
            )
            .weak(),
        );
    }
}

fn show_manual_summary(ui: &mut egui::Ui, state: &GuiState) {
    let count = state
        .settings
        .remotes
        .iter()
        .filter(|remote| !remote.trim().is_empty())
        .count();
    ui.label(egui::RichText::new("Manual one-off upload").strong());
    ui.label(
        egui::RichText::new(format!(
            "{count} destination(s) configured. Open Advanced to review or change the manual upload policy."
        ))
        .weak(),
    );
}

pub(crate) fn show_manual_destinations(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(
        egui::RichText::new(
            "Manual destinations are intended for one-off uploads. New archive data is still restricted to safe rclone crypt remotes.",
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
