use crate::doctor::{run_checks, Diagnostic};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{section_header, status_badge, StatusTone};
use eframe::egui;

#[derive(Debug, Default)]
pub(crate) struct SystemForm {
    pub(crate) reports: Vec<Diagnostic>,
    pub(crate) error: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, _task: &mut TaskRunner) {
    section_header(
        ui,
        "Diagnostics",
        Some("Check rclone, providers, pools, configuration, and rebuildable local metadata."),
    );

    if ui.button("Run doctor").clicked() {
        state.system.reports = run_checks(&state.settings.rclone);
        state.system.error = None;
    }

    if state.system.reports.is_empty() {
        ui.label(egui::RichText::new("No diagnostic result yet.").weak());
        return;
    }

    let failures = state.system.reports.iter().filter(|report| report.status == "fail").count();
    let warnings = state.system.reports.iter().filter(|report| report.status == "warn").count();
    ui.horizontal(|ui| {
        if failures > 0 {
            status_badge(ui, &format!("{failures} failure(s)"), StatusTone::Error);
        } else if warnings > 0 {
            status_badge(ui, &format!("{warnings} warning(s)"), StatusTone::Warning);
        } else {
            status_badge(ui, "Checks passed", StatusTone::Success);
        }
    });

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("doctor-report-grid").striped(true).show(ui, |ui| {
            ui.strong("Status");
            ui.strong("Check");
            ui.strong("Result");
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
}
