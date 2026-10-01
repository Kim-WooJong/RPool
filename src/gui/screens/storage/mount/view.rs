//! Drive page: status and actions first, then connection and capacity side by
//! side, anything that needs attention, and the log. Options, imports and
//! maintenance are separate tabs.
use crate::gui::i18n::tr;
use crate::gui::state::{DriveTab, GuiState};
use crate::gui::theme;
use eframe::egui;

/// Inputs whose change invalidates the shown capacity and sync status.
fn inputs(state: &GuiState) -> impl PartialEq {
    let f = &state.mount;
    (f.pool.clone(), f.workspace.clone(), f.manifests.clone())
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let before = inputs(state);
    let before_pool = state.mount.pool.clone();
    theme::tabs(
        ui,
        &mut state.mount.tab,
        &[
            (DriveTab::Drive, tr("Drive")),
            (DriveTab::Options, tr("Options")),
            (DriveTab::Import, tr("Import")),
            (DriveTab::Maintenance, tr("Maintenance")),
        ],
    );
    theme::page_body(ui, "drive", |ui| match state.mount.tab {
        DriveTab::Drive => overview(ui, state),
        DriveTab::Options => super::options::show(ui, state),
        DriveTab::Import => super::import::show(ui, state),
        DriveTab::Maintenance => super::maintenance::show(ui, state),
    });
    // Selecting another pool restores that pool's session as it was.
    if inputs(state) != before && state.mount.pool == before_pool {
        state.mount.session.capacity = None;
        state.mount.session.pool_status = None;
        state.mount.session.layout_status = None;
    }
}

fn overview(ui: &mut egui::Ui, state: &mut GuiState) {
    super::sessions_strip::show(ui, state);
    super::status_bar::show(ui, state);
    super::layout_notice::show(ui, &state.mount);
    ui.add_space(theme::SUBSECTION_GAP);
    theme::two_up(ui, state, super::drive_section::show, |ui, state| {
        super::capacity_panel::show(ui, state);
        super::identity_summary::show(ui, state);
    });
    super::cache_recovery::show(ui, &mut state.mount);
    super::conflicts::show(ui, &state.mount);
    super::log::show(ui, &state.mount);
}
