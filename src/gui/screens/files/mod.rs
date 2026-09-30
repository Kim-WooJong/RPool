#[path = "inventory/mod.rs"]
pub(crate) mod inventory;
pub(crate) mod restore;
pub(crate) mod status;
#[path = "upload/mod.rs"]
pub(crate) mod upload;
pub(crate) mod verify;

pub(crate) use inventory::InventoryForm;
pub(crate) use restore::RestoreForm;
pub(crate) use status::StatusForm;
pub(crate) use upload::UploadForm;
pub(crate) use verify::VerifyForm;

use crate::gui::state::{FilesSection, GuiState};
use crate::gui::task::TaskRunner;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    crate::gui::theme::tabs(
        ui,
        &mut state.files_section,
        &[
            (FilesSection::Inventory, "Library"),
            (FilesSection::Upload, "Upload"),
            (FilesSection::Restore, "Restore"),
        ],
    );
    match state.files_section {
        FilesSection::Inventory => inventory::show(ui, state, task),
        FilesSection::Upload => upload::show(ui, state, task),
        FilesSection::Restore => restore::show(ui, state, task),
    }
}
