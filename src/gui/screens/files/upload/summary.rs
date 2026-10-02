//! The "Upload summary" card: readiness badge, files / target / policy /
//! storage lines, and the preflight issues and warnings.

use super::validation::UploadPreflight;
use crate::gui::i18n::tr;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// Draws the summary of `report` (from `validation::evaluate`). Called by
/// `upload::show`.
pub(crate) fn show(ui: &mut egui::Ui, report: &UploadPreflight) {
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("Upload summary")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if report.in_progress {
                        status_badge(ui, tr("Uploading"), StatusTone::Warning);
                    } else if report.ready {
                        status_badge(ui, tr("Ready"), StatusTone::Success);
                    } else {
                        status_badge(ui, tr("Needs attention"), StatusTone::Warning);
                    }
                });
            });

            ui.add_space(6.0);
            egui::Grid::new("upload-preflight-summary")
                .num_columns(2)
                .spacing([18.0, 5.0])
                .show(ui, |ui| {
                    summary_row(ui, tr("Files"), &report.files_label);
                    summary_row(ui, tr("Target"), &report.target_label);
                    summary_row(ui, tr("Policy"), &report.policy_label);
                    summary_row(ui, tr("Storage"), &report.destinations_label);
                });

            if !report.issues.is_empty() {
                ui.add_space(8.0);
                let (_, foreground) = theme::error_colors(ui.visuals().dark_mode);
                for issue in &report.issues {
                    ui.label(egui::RichText::new(format!("• {issue}")).color(foreground));
                }
            }

            if !report.warnings.is_empty() {
                ui.add_space(6.0);
                let (_, foreground) = theme::warning_colors(ui.visuals().dark_mode);
                for warning in &report.warnings {
                    ui.label(egui::RichText::new(format!("• {warning}")).color(foreground));
                }
            }
        });
}

/// One label/value row of the summary grid.
fn summary_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).weak());
    ui.label(value);
    ui.end_row();
}
