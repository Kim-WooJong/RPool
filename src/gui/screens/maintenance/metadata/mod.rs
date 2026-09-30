mod inventory;
mod manifest;

use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::section_header;
use eframe::egui;

#[derive(Debug, Default)]
pub(crate) struct ManifestForm {
    pub(crate) reference: String,
    pub(crate) pool_name: String,
    pub(crate) recovery_archive_id: String,
    pub(crate) recovery_output: String,
    pub(crate) recovery_remotes: Vec<String>,
    pub(crate) manual_remote: String,
    pub(crate) inventory_directory: String,
    pub(crate) inventory_manifest: String,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_header(
        ui,
        "Metadata",
        Some("Maintain manifest replicas and rebuild the local inventory cache from manifests."),
    );

    manifest::show(ui, state, task);
    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SECTION_GAP);
    inventory::show(ui, state, task);

    if let Some(error) = &state.manifest.error {
        let (_, color) = crate::gui::theme::error_colors(ui.visuals().dark_mode);
        ui.label(egui::RichText::new(error).color(color));
    }
    if let Some(notice) = &state.manifest.notice {
        ui.small(notice);
    }
}
