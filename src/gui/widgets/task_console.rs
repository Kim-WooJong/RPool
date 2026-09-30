use crate::gui::task::{LogKind, TaskRunner};
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// The operation console. `full` draws progress and the log; otherwise one
/// status line. Returns true when the user toggles between the two.
pub(crate) fn task_console(ui: &mut egui::Ui, task: &mut TaskRunner, full: bool) -> bool {
    let mut toggle = false;
    ui.horizontal_wrapped(|ui| {
        toggle = ui
            .small_button(if full { "Hide" } else { "Show" })
            .on_hover_text(if full {
                "Collapse the console"
            } else {
                "Show the console"
            })
            .clicked();
        ui.label(egui::RichText::new("Operation console").strong());
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

    if !full {
        return toggle;
    }
    if let Some(info) = task.current_task().or(task.last_task()) {
        crate::gui::widgets::progress_view(ui, &info.progress);
    }

    if let Some(command) = task.command_preview() {
        ui.collapsing("Command", |ui| {
            ui.monospace(command);
        });
    }

    egui::ScrollArea::vertical()
        .id_salt("task-console-log")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .max_height(ui.available_height())
        .show(ui, |ui| {
            if task.logs().is_empty() {
                ui.add_space(12.0);
                ui.label(egui::RichText::new("No operations yet").strong());
                ui.label(egui::RichText::new("Start an operation from Drive, Files, Storage or Health. Output appears here.").weak());
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
    toggle
}
