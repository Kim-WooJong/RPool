use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug)]
pub(crate) struct JobsForm {
    pub(crate) history_limit: usize,
    pub(crate) history_keep: usize,
    pub(crate) error: Option<String>,
}

impl Default for JobsForm {
    fn default() -> Self {
        Self {
            history_limit: 50,
            history_keep: 500,
            error: None,
        }
    }
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    crate::gui::theme::page_header(
        ui,
        tr("Activity"),
        Some(tr("The current operation and the recorded task history.")),
    );
    crate::gui::theme::card_section(
        ui,
        tr("Current operation"),
        None,
        |_| {},
        |ui| {
            if task.is_running() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.strong(task.task_name().unwrap_or(tr("Operation")));
                });
                if let Some(command) = task.command_preview() {
                    ui.monospace(command);
                }
            } else if let Some(outcome) = task.last_outcome() {
                let status = if outcome.cancelled {
                    tr("Cancelled")
                } else if outcome.success {
                    tr("Completed")
                } else {
                    tr("Failed")
                };
                ui.label(trf("Last operation: {status}", &[("status", &status)]));
                if let Some(code) = outcome.code {
                    ui.small(trf("Exit code: {code}", &[("code", &code)]));
                }
            } else {
                ui.label(tr("No operation is currently running."));
            }
            crate::gui::theme::hint(
                ui,
                tr("Raw operation output remains available in the console below."),
            );
        },
    );
    let size = egui::vec2(ui.available_width(), ui.available_height().max(200.0));
    crate::gui::theme::fixed_pane(ui, "jobs-history-section", size, |ui| {
        ui.label(
            egui::RichText::new(tr("Task history"))
                .size(crate::gui::theme::CARD_TITLE_SIZE)
                .strong(),
        );
        ui.horizontal(|ui| {
            ui.label(tr("Show latest"));
            ui.add(egui::DragValue::new(&mut state.jobs.history_limit).range(1..=100_000));
            if ui
                .add_enabled(!task.is_running(), egui::Button::new(tr("Show history")))
                .clicked()
            {
                let args = [
                    OsString::from("history"),
                    OsString::from("list"),
                    OsString::from("--limit"),
                    OsString::from(state.jobs.history_limit.to_string()),
                ];
                state.jobs.error = task
                    .start_rpool("History", &state.settings.rclone, args)
                    .err();
            }
        });
        ui.horizontal(|ui| {
            ui.label(tr("Keep newest"));
            ui.add(egui::DragValue::new(&mut state.jobs.history_keep).range(0..=1_000_000));
            if ui
                .add_enabled(!task.is_running(), egui::Button::new(tr("Prune history")))
                .clicked()
            {
                let args = [
                    OsString::from("history"),
                    OsString::from("prune"),
                    OsString::from("--keep"),
                    OsString::from(state.jobs.history_keep.to_string()),
                ];
                state.jobs.error = task
                    .start_rpool("History prune", &state.settings.rclone, args)
                    .err();
            }
        });

        if let Some(error) = &state.jobs.error {
            ui.label(error);
        }
    });
}
