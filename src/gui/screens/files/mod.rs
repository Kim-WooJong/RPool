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
    section_tabs(ui, &mut state.files_section);
    ui.separator();

    match state.files_section {
        FilesSection::Inventory => inventory::show(ui, state, task),
        FilesSection::Upload => upload::show(ui, state, task),
        FilesSection::Restore => restore::show(ui, state, task),
        FilesSection::Verify => verify::show(ui, state, task),
        FilesSection::Status => status::show(ui, state, task),
    }
}

fn section_tabs(ui: &mut egui::Ui, section: &mut FilesSection) {
    ui.horizontal(|ui| {
        tab(ui, section, FilesSection::Inventory, "Library");
        tab(ui, section, FilesSection::Upload, "Upload");
        tab(ui, section, FilesSection::Restore, "Restore");
        tab(ui, section, FilesSection::Verify, "Verify");
        tab(ui, section, FilesSection::Status, "Status");
    });
}

fn tab(ui: &mut egui::Ui, section: &mut FilesSection, target: FilesSection, label: &str) {
    if ui.selectable_label(*section == target, label).clicked() {
        *section = target;
    }
}
