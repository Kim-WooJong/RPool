mod repair;
mod result;
mod scrub;
mod state;
mod summary;
mod target;

pub(crate) use state::IntegrityForm;

use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::section_header;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_header(
        ui,
        "Integrity",
        Some("Scrub stored shards, review degradation, and reconstruct recoverable Reed-Solomon groups."),
    );

    target::show(ui, &mut state.integrity);
    ui.add_space(theme::SECTION_GAP);
    summary::show(ui, &state.integrity);
    ui.add_space(theme::SECTION_GAP);
    ui.separator();
    ui.add_space(theme::SECTION_GAP);

    scrub::show(
        ui,
        &mut state.integrity,
        task,
        &state.settings.rclone,
        state.settings.workers,
        state.settings.retries,
    );
    ui.add_space(theme::SECTION_GAP);
    repair::show(
        ui,
        &mut state.integrity,
        task,
        &state.settings.rclone,
        state.settings.workers,
        state.settings.retries,
    );
    ui.add_space(theme::SECTION_GAP);
    result::show(ui, &state.integrity);

    if let Some(error) = &state.integrity.error {
        let (_, color) = crate::gui::theme::error_colors(ui.visuals().dark_mode);
        ui.label(egui::RichText::new(error).color(color));
    }
    if let Some(notice) = &state.integrity.notice {
        ui.small(notice);
    }
}
