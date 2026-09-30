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
    theme::page_body(ui, "health-integrity", |ui| {
        section_header(
        ui,
        "Integrity",
        Some("Scrub stored shards, review degradation, and reconstruct recoverable Reed-Solomon groups."),
    );

        let card = |ui: &mut egui::Ui, body: &mut dyn FnMut(&mut egui::Ui)| {
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                body(ui);
            });
            ui.add_space(theme::SUBSECTION_GAP + 4.0);
        };
        card(ui, &mut |ui| {
            target::show(ui, &mut state.integrity);
            ui.add_space(theme::SUBSECTION_GAP);
            summary::show(ui, &state.integrity);
        });
        let (rclone, workers, retries) = (
            state.settings.rclone.clone(),
            state.settings.workers,
            state.settings.retries,
        );
        card(ui, &mut |ui| {
            scrub::show(ui, &mut state.integrity, task, &rclone, workers, retries);
        });
        card(ui, &mut |ui| {
            repair::show(ui, &mut state.integrity, task, &rclone, workers, retries);
        });
        result::show(ui, &state.integrity);

        if let Some(error) = &state.integrity.error {
            let (_, color) = crate::gui::theme::error_colors(ui.visuals().dark_mode);
            ui.label(egui::RichText::new(error).color(color));
        }
        if let Some(notice) = &state.integrity.notice {
            ui.small(notice);
        }
    });
}
