mod data;
mod health;
mod pools;
mod providers;
mod recent_jobs;
mod summary;
mod warnings;

pub(crate) use data::{DashboardData, DashboardPool};

use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::gui::usage_refresh::UsageRefresh;
use crate::gui::widgets::{section_header, toolbar};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, usage: &mut UsageRefresh) {
    section_header(
        ui,
        "Dashboard",
        Some("Storage capacity, configured pools, and recent activity."),
    );

    toolbar(ui, |ui| {
        if ui
            .add_enabled(!usage.is_running(), egui::Button::new("Refresh"))
            .clicked()
        {
            state.dashboard.refresh();
            state.usage_error = None;
            state.providers.refresh_requested = true;
        }
        if usage.is_running() {
            ui.spinner();
            ui.label("Refreshing");
        }
    });

    ui.add_space(theme::SUBSECTION_GAP);
    // Fixed viewport cells: growing one section cannot push another off-screen.
    let gap = ui.spacing().item_spacing;
    let available = ui.available_size();
    let wide = theme::wide(ui);
    let cell = if wide {
        egui::vec2(
            ((available.x - gap.x) / 2.0).max(1.0),
            ((available.y - gap.y) / 2.0).max(220.0),
        )
    } else {
        egui::vec2(available.x, 300.0)
    };
    let row = if wide {
        egui::Layout::left_to_right(egui::Align::Min)
    } else {
        egui::Layout::top_down(egui::Align::Min)
    };
    theme::page_body(ui, "overview", |ui| {
        ui.with_layout(row, |ui| {
            crate::gui::theme::fixed_pane(ui, "dashboard-overview-scroll", cell, |ui| {
                summary::show(
                    ui,
                    &state.dashboard,
                    &state.usage_reports,
                    state.usage_error.as_deref(),
                    &state.crypt_remotes,
                    usage.is_running(),
                );
                warnings::show(
                    ui,
                    &state.dashboard,
                    &state.usage_reports,
                    state.usage_error.as_deref(),
                    &state.crypt_remotes,
                );
            });
            theme::fixed_pane_wide(ui, "dashboard-providers-scroll", cell, |ui| {
                providers::show(ui, state)
            });
        });
        ui.with_layout(row, |ui| {
            theme::fixed_pane_wide(ui, "dashboard-pools-scroll", cell, |ui| {
                pools::show(ui, state)
            });
            theme::fixed_pane_wide(ui, "dashboard-jobs-scroll", cell, |ui| {
                recent_jobs::show(ui, state)
            });
        });
    });
}
