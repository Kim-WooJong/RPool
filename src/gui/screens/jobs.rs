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
    ui.heading("Jobs");
    ui.label("Current operation and recorded task history.");
    ui.separator();

    ui.heading("Current operation");
    if task.is_running() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.strong(task.task_name().unwrap_or("Operation"));
        });
        if let Some(command) = task.command_preview() {
            ui.monospace(command);
        }
    } else if let Some(outcome) = task.last_outcome() {
        let status = if outcome.cancelled {
            "Cancelled"
        } else if outcome.success {
            "Completed"
        } else {
            "Failed"
        };
        ui.label(format!("Last operation: {status}"));
        if let Some(code) = outcome.code {
            ui.small(format!("Exit code: {code}"));
        }
    } else {
        ui.label("No operation is currently running.");
    }
    ui.small("Raw operation output remains available in the task console below.");

    ui.separator();
    ui.heading("Task history");
    ui.horizontal(|ui| {
        ui.label("Show latest");
        ui.add(egui::DragValue::new(&mut state.jobs.history_limit).range(1..=100_000));
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Show history"))
            .clicked()
        {
            let args = [
                OsString::from("history"),
                OsString::from("list"),
                OsString::from("--limit"),
                OsString::from(state.jobs.history_limit.to_string()),
            ];
            state.jobs.error = task.start_rpool("History", &state.settings.rclone, args).err();
        }
    });
    ui.horizontal(|ui| {
        ui.label("Keep newest");
        ui.add(egui::DragValue::new(&mut state.jobs.history_keep).range(0..=1_000_000));
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Prune history"))
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
}
