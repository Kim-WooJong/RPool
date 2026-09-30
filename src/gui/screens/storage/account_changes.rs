//! Storage › Account changes: the pool change migration wizard first, then
//! the manual tools (drain, reprocess, apply to a drive, recover) folded
//! under "Advanced / manual".
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::page_body(ui, "account-changes", |ui| {
        theme::page_header(
            ui,
            tr("Account changes"),
            Some(tr("When accounts leave or join a pool, or its coding changes: plan and run a migration. The manual tools below remain for special cases.")),
        );
        super::migration::card(ui, state, task);
        let header = egui::CollapsingHeader::new(tr("Advanced / manual"))
            .id_salt("account-changes-advanced")
            .open(Some(state.migration.show_advanced))
            .show(ui, |ui| {
                theme::hint(ui, tr("Single-step tools: drain one account, reprocess chosen archives, apply pool changes to a mounted drive, or recover a drive into a new pool."));
                super::providers::drain_card(ui, state, task);
                theme::card_section(ui, tr("Reprocess archives for a changed pool"), Some(tr("Re-encodes selected archives with a new pool policy. No old provider data is deleted.")), |_| {}, |ui| {
                    super::reprocess::show(ui, state, task);
                });
                theme::two_up(
                    ui,
                    state,
                    super::mount::transitions::apply_card,
                    super::mount::transitions::recover_card,
                );
            });
        if header.header_response.clicked() {
            state.migration.show_advanced = !state.migration.show_advanced;
        }
    });
}
