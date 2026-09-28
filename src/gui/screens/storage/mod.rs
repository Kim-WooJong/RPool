pub(crate) mod mount;
mod pool_picker;
pub(crate) mod pools;
pub(crate) mod providers;
pub(crate) mod reprocess;

pub(crate) use pools::PoolForm;
pub(crate) use providers::ProviderForm;

use crate::gui::state::{GuiState, StorageSection};
use crate::gui::task::TaskRunner;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_tabs(ui, &mut state.storage_section);
    ui.separator();

    match state.storage_section {
        StorageSection::Providers => providers::show(ui, state, task),
        StorageSection::Pools => pools::show(ui, state, task),
        StorageSection::Reprocess => reprocess::show(ui, state, task),
        StorageSection::Mount => mount::show(ui, state),
    }
}

fn section_tabs(ui: &mut egui::Ui, section: &mut StorageSection) {
    ui.horizontal(|ui| {
        tab(ui, section, StorageSection::Providers, "Providers");
        tab(ui, section, StorageSection::Pools, "Pools");
        tab(ui, section, StorageSection::Reprocess, "Reprocess data");
        tab(ui, section, StorageSection::Mount, "Mount drive");
    });
}

fn tab(ui: &mut egui::Ui, section: &mut StorageSection, target: StorageSection, label: &str) {
    if ui.selectable_label(*section == target, label).clicked() {
        *section = target;
    }
}
