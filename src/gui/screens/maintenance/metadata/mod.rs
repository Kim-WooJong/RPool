//! Maintenance › Metadata: manifest replica tools (`rpool manifest
//! verify|replicate|recover`) and the local inventory index (`rpool inventory
//! add|rebuild`).

/// Inventory card: add one manifest or rebuild the index from a folder.
mod inventory;
/// Manifest replica cards: where replicas live, check/repair, recover.
mod manifest;

use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::section_header;
use eframe::egui;

/// Inputs of the Metadata tab, in `GuiState::manifest`.
#[derive(Debug, Default)]
pub(crate) struct ManifestForm {
    /// Reference manifest for verify / replicate (local file or remote path).
    pub(crate) reference: String,
    /// Pool whose remotes hold the replicas (`""` = use explicit remotes, or the
    /// manifest's own providers).
    pub(crate) pool_name: String,
    /// Archive ID whose lost manifest to recover.
    pub(crate) recovery_archive_id: String,
    /// Optional local file for the recovered manifest (`--output`).
    pub(crate) recovery_output: String,
    /// Explicit crypt remotes used when no pool is chosen (`--remote`).
    pub(crate) recovery_remotes: Vec<String>,
    /// Text field buffer of the remote selector.
    pub(crate) manual_remote: String,
    /// Folder of manifests for "Rebuild inventory".
    pub(crate) inventory_directory: String,
    /// Manifest (local file or remote object) for "Add to inventory".
    pub(crate) inventory_manifest: String,
    /// Start error of the last action.
    pub(crate) error: Option<String>,
    /// Result message (set after an inventory rebuild finishes).
    pub(crate) notice: Option<String>,
}

/// Draws the Metadata tab: messages, manifest cards and the inventory card.
/// Called by `maintenance::show`.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::page_body(ui, "health-metadata", |ui| {
        section_header(
            ui,
            tr("Metadata"),
            Some(tr("Manifest replicas are what an archive is rebuilt from; the inventory is this PC's index of archives.")),
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
            tr("Inventory"),
            Some(tr(
                "Add archives to this PC's index, or rebuild it from a folder of manifests.",
            )),
            |_| {},
            |ui| {
                inventory::show(ui, state, task);
            },
        );
    });
}
