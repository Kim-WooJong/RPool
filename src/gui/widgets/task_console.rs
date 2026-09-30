use crate::gui::i18n::{tr, trf};
use crate::gui::task::{LogKind, TaskRunner};
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// The operation console. `full` draws progress and the log; otherwise one
/// status line. Returns true when the user toggles between the two.
pub(crate) fn task_console(ui: &mut egui::Ui, task: &mut TaskRunner, full: bool) -> bool {
    let mut toggle = false;
    ui.horizontal_wrapped(|ui| {
        toggle = ui
            .small_button(if full { tr("Hide") } else { tr("Show") })
            .on_hover_text(if full {
                tr("Collapse the console")
            } else {
                tr("Show the console")
            })
            .clicked();
        ui.label(egui::RichText::new(tr("Operation console")).strong());
        if task.is_running() {
            status_badge(ui, tr("Running"), StatusTone::Warning);
            ui.label(task.task_name().unwrap_or(tr("operation")));
            if ui.button(tr("Cancel")).clicked() {
                task.cancel();
            }
        } else if let Some(outcome) = task.last_outcome() {
            if outcome.cancelled {
                status_badge(ui, tr("Cancelled"), StatusTone::Neutral);
            } else if outcome.success {
                status_badge(ui, tr("Completed"), StatusTone::Success);
            } else {
                status_badge(ui, tr("Failed"), StatusTone::Error);
                if let Some(code) = outcome.code {
                    ui.label(trf("Exit code {code}", &[("code", &code)]));
                }
            }
            if ui.button(tr("Clear")).clicked() {
                task.clear_log();
            }
        } else {
            status_badge(ui, tr("Idle"), StatusTone::Neutral);
        }
    });

    if !full {
        return toggle;
    }
    if let Some(info) = task.current_task().or(task.last_task()) {
        crate::gui::widgets::progress_view(ui, &info.progress);
    }

    if let Some(command) = task.command_preview() {
        ui.collapsing(tr("Command"), |ui| {
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
                ui.label(egui::RichText::new(tr("No operations yet")).strong());
                ui.label(egui::RichText::new(tr("Start an operation from Drive, Files, Storage or Health. Output appears here.")).weak());
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
