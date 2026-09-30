//! Drive page: status and actions first, then connection and capacity side by
//! side, anything that needs attention, and the log. Options, history
//! cleanup, imports and maintenance are separate tabs.
use crate::gui::state::{DriveTab, GuiState};
use crate::gui::theme;
use eframe::egui;

/// Inputs whose change invalidates the shown capacity and sync status.
fn inputs(state: &GuiState) -> impl PartialEq {
    let f = &state.mount;
    (
        f.pool.clone(),
        f.workspace.clone(),
        f.shared_root.clone(),
        f.manifests.clone(),
        (
            f.virtual_drive,
            f.pool_sync,
            f.pool_retention,
            f.bounded_shared,
        ),
        (f.shared_coordinator, f.shared_keep_previous),
    )
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let before = inputs(state);
    let history = if state.mount.retention_allowed() {
        "History cleanup"
    } else {
        "History"
    };
    theme::tabs(
        ui,
        &mut state.mount.tab,
        &[
            (DriveTab::Drive, "Drive"),
            (DriveTab::Options, "Options"),
            (DriveTab::History, history),
            (DriveTab::Import, "Import"),
            (DriveTab::Maintenance, "Maintenance"),
        ],
    );
    theme::page_body(ui, "drive", |ui| match state.mount.tab {
        DriveTab::Drive => overview(ui, state),
        DriveTab::Options => super::options::show(ui, state),
        DriveTab::History => super::cleanup::show(ui, state),
        DriveTab::Import => super::import::show(ui, state),
        DriveTab::Maintenance => super::maintenance::show(ui, state),
    });
    if inputs(state) != before {
        state.mount.capacity = None;
        state.mount.pool_status = None;
    }
}

fn overview(ui: &mut egui::Ui, state: &mut GuiState) {
    super::status_bar::show(ui, state);
    ui.add_space(theme::SUBSECTION_GAP);
    theme::two_up(ui, state, super::drive_section::show, |ui, state| {
        super::capacity_panel::show(ui, state);
        super::identity_summary::show(ui, state);
    });
    super::cache_recovery::show(ui, &mut state.mount);
    super::conflicts::show(ui, &state.mount);
    super::log::show(ui, &state.mount);
}
