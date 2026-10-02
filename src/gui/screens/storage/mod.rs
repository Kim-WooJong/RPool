//! Storage page: tabs for Providers (accounts and crypt remotes), Pools
//! (pool definitions and their cards) and Account changes (migration and
//! manual tools). Rendered by `gui::app` for the Storage page.
mod account_changes;
mod cleanup_section;
pub(crate) mod metadata_card;
pub(crate) mod migration;
pub(crate) mod mount;
/// Dialog that picks the encrypted providers of a pool draft (Pools, Reprocess).
mod pool_picker;
/// Pools tab: create, edit and inspect pool definitions.
pub(crate) mod pools;
/// Providers tab: rclone remotes, crypt provisioning, limits and drain.
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

/// Renders the Storage page: the section tab strip and the selected section.
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
