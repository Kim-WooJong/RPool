use crate::gui::screens::dashboard::DashboardData;
use crate::gui::state::{GuiState, Page};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::models::TaskRecord;
use crate::presentation::relative_age;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Recent jobs")
                .size(theme::SECTION_TITLE_SIZE)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link("Open Jobs").clicked() {
                state.page = Page::Jobs;
            }
        });
    });
    ui.add_space(theme::SUBSECTION_GAP);

    show_rows(ui, &state.dashboard);
}

fn show_rows(ui: &mut egui::Ui, data: &DashboardData) {
    if data.recent_jobs.is_empty() {
        ui.label("No recorded jobs yet.");
        return;
    }

    egui::Grid::new("dashboard-recent-jobs")
        .num_columns(4)
        .striped(true)
        .spacing([18.0, 7.0])
        .show(ui, |ui| {
            ui.strong("Operation");
            ui.strong("Target");
            ui.strong("Result");
            ui.strong("Finished");
            ui.end_row();

            for record in &data.recent_jobs {
                job_row(ui, record);
            }
        });
}

fn job_row(ui: &mut egui::Ui, record: &TaskRecord) {
    ui.label(display_operation(&record.operation));
    ui.label(truncate_target(record.target.as_deref().unwrap_or("-"), 48));

    if record.status.eq_ignore_ascii_case("success") {
        status_badge(ui, "Success", StatusTone::Success);
    } else {
        status_badge(ui, "Failed", StatusTone::Error);
    }
    ui.monospace(relative_age(record.finished_unix));
    ui.end_row();
}

fn display_operation(value: &str) -> String {
    value
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_target(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let prefix: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{prefix}…")
}
