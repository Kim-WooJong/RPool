//! Providers pane of the overview page: capacity per provider.
use crate::gui::i18n::tr;
use crate::gui::state::{GuiState, Page, StorageSection};
use crate::gui::theme;
use crate::gui::widgets::{capacity_bar_sized, status_badge, StatusTone};
use crate::models::QuotaReport;
use crate::presentation::format_optional_bytes;
use eframe::egui;

/// Draw the provider capacity table with a link to Storage > Providers.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    section_title(
        ui,
        state,
        tr("Providers"),
        tr("Open Storage"),
        StorageSection::Providers,
    );

    if state.usage_reports.is_empty() {
        ui.label(tr("No provider capacity data is available yet."));
        return;
    }

    egui::Grid::new("dashboard-providers")
        .num_columns(5)
        .striped(true)
        .spacing([18.0, 7.0])
        .show(ui, |ui| {
            ui.strong(tr("Provider"));
            ui.strong(tr("Usage"));
            ui.strong(tr("Used"));
            ui.strong(tr("Free"));
            ui.strong(tr("Status"));
            ui.end_row();

            for report in &state.usage_reports {
                provider_row(ui, report);
            }
        });
}

/// One provider row: name, usage bar, used/free bytes and availability.
fn provider_row(ui: &mut egui::Ui, report: &QuotaReport) {
    ui.label(&report.remote);

    let ratio = match (report.used, report.total) {
        (Some(used), Some(total)) if total > 0 => {
            Some((used as f64 / total as f64).clamp(0.0, 1.0) as f32)
        }
        _ => None,
    };
    let text = report
        .used_percent
        .map(|value| format!("{value:.0}%"))
        .unwrap_or_else(|| tr("n/a").to_string());
    capacity_bar_sized(ui, ratio, &text, 180.0);

    ui.monospace(format_optional_bytes(report.used));
    ui.monospace(format_optional_bytes(report.free));
    if report.error.is_some() {
        status_badge(ui, tr("Unavailable"), StatusTone::Error);
    } else {
        status_badge(ui, tr("Available"), StatusTone::Success);
    }
    ui.end_row();
}

/// Pane title with a right-aligned link that opens a Storage section.
fn section_title(
    ui: &mut egui::Ui,
    state: &mut GuiState,
    title: &str,
    action: &str,
    section: StorageSection,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .size(theme::SECTION_TITLE_SIZE)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link(action).clicked() {
                state.page = Page::Storage;
                state.storage_section = section;
            }
        });
    });
    ui.add_space(theme::SUBSECTION_GAP);
}
