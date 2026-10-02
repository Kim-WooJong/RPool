//! Account identities are edited in Pools, next to the capacity estimate
//! they drive; the Mount screen only shows whether they are complete.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::{GuiState, StorageSection};
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// One-line status of how many capacity targets have a declared account
/// identity, with a link to edit them in Pools; hidden without a capacity snapshot.
pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let Some(capacity) = &state.mount.session.capacity else {
        return;
    };
    let total = capacity.targets.len();
    let declared = capacity.targets.iter().filter(|t| t.declared).count();
    let mut open = false;
    ui.horizontal(|ui| {
        if declared == total && total > 0 {
            status_badge(ui, tr("Identities declared"), StatusTone::Success);
        } else {
            status_badge(ui, tr("Identities incomplete"), StatusTone::Warning);
        }
        ui.label(trf("{declared} of {total} measured accounts declared", &[("declared", &declared), ("total", &total)]));
        open = ui
            .button(tr("Edit in Pools"))
            .on_hover_text(tr("Opens this pool in Storage › Pools, where identities are set next to the capacity estimate."))
            .clicked();
    });
    if open {
        state.pools.selected = state.mount.pool.clone();
        super::super::pools::load_selected(state);
        state.page = crate::gui::state::Page::Storage;
        state.storage_section = StorageSection::Pools;
    }
}
