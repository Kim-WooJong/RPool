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
                let dark = ui.visuals().dark_mode;
                for line in task.logs() {
                    let text = egui::RichText::new(format!(
                        "[{}] {}",
                        prefix(line.kind),
                        line.text
                    ))
                    .monospace();
                    ui.label(if is_error_line(&line.text) {
                        text.color(crate::gui::theme::error_colors(dark).1)
                    } else {
                        text
                    });
                }
            }
        });
    toggle
}

/// `OUT` = the result on stdout, `LOG` = progress, status and errors on
/// stderr (a stream, not a verdict), `SYS` = the GUI's own lines.
fn prefix(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Stdout => "OUT",
        LogKind::Stderr => "LOG",
        LogKind::System => "SYS",
    }
}

/// A line that reports a failure, shown in the error color: `Error: …`,
/// `… failed: …`, `FAILED`, `[ERROR]`.
fn is_error_line(text: &str) -> bool {
    let line = text.trim_start();
    let lower = line.to_ascii_lowercase();
    lower.starts_with("error")
        || lower.starts_with("[error")
        || line.contains("FAILED")
        || lower.contains(": failed")
        || lower.contains("failed:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_on_stderr_is_a_log_line_not_an_error() {
        assert_eq!(prefix(LogKind::Stderr), "LOG");
        assert!(!is_error_line(
            "Testing filen_1_crypt: (3/6): uploading 0/2 files, 57.1/128.0 MiB"
        ));
        assert!(!is_error_line("Tested koofr_1_crypt: (4/6): ok"));
        assert!(is_error_line("Error: some accounts did not answer"));
        assert!(is_error_line(
            "Tested filen_1_crypt: (3/6): failed: upload: rclone rate limited"
        ));
        assert!(is_error_line(
            "filen_1                  FAILED: rate limited"
        ));
    }
}
