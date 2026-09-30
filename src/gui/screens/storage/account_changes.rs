//! Storage › Account changes: everything used when a storage account is
//! added, replaced or lost, in the order it is done.
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::page_body(ui, "account-changes", |ui| {
        theme::page_header(
            ui,
            "Account changes",
            Some("Moving data off an account, re-encoding archives for a changed pool, and bringing a drive up to date. Work top to bottom."),
        );
        super::providers::drain_card(ui, state, task);
        theme::card_section(ui, "Reprocess archives for a changed pool", Some("Re-encodes selected archives with a new pool policy. No old provider data is deleted."), |_| {}, |ui| {
            super::reprocess::show(ui, state, task);
        });
        theme::two_up(
            ui,
            state,
            super::mount::transitions::apply_card,
            super::mount::transitions::recover_card,
        );
    });
}
