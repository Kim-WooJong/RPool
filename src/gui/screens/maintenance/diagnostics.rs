use crate::doctor::{run_checks, Diagnostic};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{section_header, status_badge, StatusTone};
use eframe::egui;

#[derive(Debug, Default)]
pub(crate) struct SystemForm {
    pub(crate) reports: Vec<Diagnostic>,
    pub(crate) error: Option<String>,
    /// Checks run off the UI thread: probing rclone can take seconds.
    pending: Option<std::sync::mpsc::Receiver<Vec<Diagnostic>>>,
}

fn start(form: &mut SystemForm, rclone: Option<String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reports = match rclone {
            Some(rclone) => run_checks(&rclone),
            None => crate::doctor::run_checks_with(None, None),
        };
        let _ = tx.send(reports);
    });
    form.pending = Some(rx);
    form.error = None;
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, _task: &mut TaskRunner) {
    crate::gui::theme::page_body(ui, "health-diagnostics", |ui| {
        section_header(
            ui,
            tr("Diagnostics"),
            Some(tr(
                "Check rclone, providers, pools, configuration, and rebuildable local metadata.",
            )),
        );

        if let Some(rx) = &state.system.pending {
            match rx.try_recv() {
                Ok(reports) => {
                    state.system.reports = reports;
                    state.system.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    state.system.pending = None;
                    state.system.error =
                        Some(tr("Diagnostics stopped unexpectedly; run them again.").into());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let running = state.system.pending.is_some();
        ui.horizontal_wrapped(|ui| {
            if crate::gui::theme::primary_button(ui, !running, tr("Run doctor")).clicked() {
                start(&mut state.system, Some(state.settings.rclone.clone()));
            }
            if ui
                .add_enabled(!running, egui::Button::new(tr("Local checks only")))
                .on_hover_text(tr(
                    "Skips rclone and providers: configuration, pools and local metadata only.",
                ))
                .clicked()
            {
                start(&mut state.system, None);
            }
            if running {
                ui.spinner();
                ui.label(tr("Checking…"));
            }
        });
        if let Some(error) = &state.system.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        if state.system.reports.is_empty() {
            ui.label(egui::RichText::new(tr("No diagnostic result yet.")).weak());
            return;
        }

        let failures = state
            .system
            .reports
            .iter()
            .filter(|report| report.status == "fail")
            .count();
        let warnings = state
            .system
            .reports
            .iter()
            .filter(|report| report.status == "warn")
            .count();
        ui.horizontal(|ui| {
            if failures > 0 {
                status_badge(
                    ui,
                    &trf("{n} failure(s)", &[("n", &failures)]),
                    StatusTone::Error,
                );
            } else if warnings > 0 {
                status_badge(
                    ui,
                    &trf("{n} warning(s)", &[("n", &warnings)]),
                    StatusTone::Warning,
                );
            } else {
                status_badge(ui, tr("Checks passed"), StatusTone::Success);
            }
        });

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("doctor-report-grid")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong(tr("Status"));
                    ui.strong(tr("Check"));
                    ui.strong(tr("Result"));
                    ui.end_row();
                    for report in &state.system.reports {
                        let tone = match report.status.as_str() {
                            "ok" => StatusTone::Success,
                            "warn" => StatusTone::Warning,
                            "fail" => StatusTone::Error,
                            _ => StatusTone::Neutral,
                        };
                        status_badge(ui, &report.status.to_uppercase(), tone);
                        ui.monospace(&report.check);
                        ui.label(&report.message);
                        ui.end_row();
                    }
                });
        });
    });
}
