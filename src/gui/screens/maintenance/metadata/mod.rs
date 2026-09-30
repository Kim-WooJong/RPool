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
    theme::page_body(ui, "health-metadata", |ui| {
        section_header(
            ui,
            "Metadata",
            Some("Manifest replicas are what an archive is rebuilt from; the inventory is this PC's index of archives."),
        );
        if let Some(error) = &state.manifest.error {
            let (_, color) = crate::gui::theme::error_colors(ui.visuals().dark_mode);
            ui.label(egui::RichText::new(error).color(color));
        }
        if let Some(notice) = &state.manifest.notice {
            ui.label(notice);
        }
        manifest::show(ui, state, task);
        theme::card_section(
            ui,
            "Inventory",
            Some("Add archives to this PC's index, or rebuild it from a folder of manifests."),
            |_| {},
            |ui| {
                inventory::show(ui, state, task);
            },
        );
    });
}
