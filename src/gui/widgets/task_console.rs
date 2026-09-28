use crate::gui::task::{LogKind, TaskRunner};
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

pub(crate) fn task_console(ui: &mut egui::Ui, task: &mut TaskRunner) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.heading("Operation console");
        if task.is_running() {
            status_badge(ui, "Running", StatusTone::Warning);
            ui.label(task.task_name().unwrap_or("operation"));
            if ui.button("Cancel").clicked() {
                task.cancel();
            }
        } else if let Some(outcome) = task.last_outcome() {
            if outcome.cancelled {
                status_badge(ui, "Cancelled", StatusTone::Neutral);
            } else if outcome.success {
                status_badge(ui, "Completed", StatusTone::Success);
            } else {
                status_badge(ui, "Failed", StatusTone::Error);
                if let Some(code) = outcome.code {
                    ui.label(format!("Exit code {code}"));
                }
            }
            if ui.button("Clear").clicked() {
                task.clear_log();
            }
        } else {
            status_badge(ui, "Idle", StatusTone::Neutral);
        }
    });

    if let Some(command) = task.command_preview() {
        ui.collapsing("Command", |ui| {
            ui.monospace(command);
        });
    }

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .max_height(260.0)
        .show(ui, |ui| {
            if task.logs().is_empty() {
                ui.label("No task output yet.");
            } else {
                for line in task.logs() {
                    let prefix = match line.kind {
                        LogKind::Stdout => "OUT",
                        LogKind::Stderr => "ERR",
                        LogKind::System => "SYS",
                    };
                    ui.monospace(format!("[{prefix}] {}", line.text));
                }
            }
        });
}
