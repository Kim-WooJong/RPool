//! Mount screen layout.
use crate::gui::state::GuiState;
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
    egui::ScrollArea::vertical()
        .id_salt("mount-page")
        .show(ui, |ui| {
            let before = inputs(state);
            super::status_bar::show(ui, state);
            ui.add_space(theme::SECTION_GAP);
            super::drive_section::show(ui, state);
            ui.add_space(theme::SECTION_GAP);
            super::capacity_panel::show(ui, state);
            ui.add_space(theme::SUBSECTION_GAP);
            super::identity_summary::show(ui, state);
            super::conflicts::show(ui, &state.mount);
            super::advanced::show(ui, state);
            super::log::show(ui, &state.mount);
            if inputs(state) != before {
                state.mount.capacity = None;
                state.mount.pool_status = None;
            }
        });
}
