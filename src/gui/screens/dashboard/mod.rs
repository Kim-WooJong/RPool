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
            usage.start(state.settings.rclone.clone(), state.settings.workers);
        }
        if usage.is_running() {
            ui.spinner();
            ui.label("Refreshing");
        }
    });

    ui.add_space(theme::SUBSECTION_GAP);
    egui::ScrollArea::vertical().show(ui, |ui| {
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

        providers::show(ui, state);
        ui.add_space(theme::SECTION_GAP);
        ui.separator();
        ui.add_space(theme::SECTION_GAP);

        pools::show(ui, state);
        ui.add_space(theme::SECTION_GAP);
        ui.separator();
        ui.add_space(theme::SECTION_GAP);

        recent_jobs::show(ui, state);
        ui.add_space(theme::SECTION_GAP);
    });
}
