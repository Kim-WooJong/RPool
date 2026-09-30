//! Account identities are edited in Pools, next to the capacity estimate
//! they drive; the Mount screen only shows whether they are complete.
use crate::gui::state::{GuiState, StorageSection};
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let Some(capacity) = &state.mount.capacity else {
        return;
    };
    let total = capacity.targets.len();
    let declared = capacity.targets.iter().filter(|t| t.declared).count();
    let mut open = false;
    ui.horizontal(|ui| {
        if declared == total && total > 0 {
            status_badge(ui, "Identities declared", StatusTone::Success);
        } else {
            status_badge(ui, "Identities incomplete", StatusTone::Warning);
        }
        ui.label(format!("{declared} of {total} measured accounts declared"));
        open = ui
            .button("Edit in Pools")
            .on_hover_text("Opens this pool in Storage › Pools, where identities are set next to the capacity estimate.")
            .clicked();
    });
    if open {
        state.pools.selected = state.mount.pool.clone();
        super::super::pools::load_selected(state);
        state.page = crate::gui::state::Page::Storage;
        state.storage_section = StorageSection::Pools;
    }
}
