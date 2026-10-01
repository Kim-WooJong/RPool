mod account_changes;
pub(crate) mod metadata_card;
pub(crate) mod migration;
pub(crate) mod mount;
mod pool_picker;
pub(crate) mod pools;
pub(crate) mod providers;
pub(crate) mod reprocess;
pub(crate) mod retention_card;
pub(crate) mod speed_test;

pub(crate) use pools::PoolForm;
pub(crate) use providers::ProviderForm;

use crate::gui::i18n::tr;
use crate::gui::state::{GuiState, StorageSection};
use crate::gui::task::TaskRunner;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    crate::gui::theme::tabs(
        ui,
        &mut state.storage_section,
        &[
            (StorageSection::Providers, tr("Providers")),
            (StorageSection::Pools, tr("Pools")),
            (StorageSection::Changes, tr("Account changes")),
        ],
    );
    match state.storage_section {
        StorageSection::Providers => providers::show(ui, state, task),
        StorageSection::Pools => pools::show(ui, state, task),
        StorageSection::Changes => account_changes::show(ui, state, task),
    }
}
