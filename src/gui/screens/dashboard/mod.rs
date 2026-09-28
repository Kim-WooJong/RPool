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
    let cell = egui::vec2(
        ((available.x - gap.x) / 2.0).max(1.0),
        ((available.y - gap.y) / 2.0).max(1.0),
    );
    ui.horizontal(|ui| {
        section(ui, "dashboard-overview-scroll", cell, |ui| {
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
        section(ui, "dashboard-providers-scroll", cell, |ui| {
            providers::show(ui, state)
        });
    });
    ui.horizontal(|ui| {
        section(ui, "dashboard-pools-scroll", cell, |ui| {
            pools::show(ui, state)
        });
        section(ui, "dashboard-jobs-scroll", cell, |ui| {
            recent_jobs::show(ui, state)
        });
    });
}

fn section(ui: &mut egui::Ui, id: &str, size: egui::Vec2, content: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    egui::ScrollArea::both()
        .id_salt(id)
        .auto_shrink([false, false])
        .min_scrolled_width(0.0)
        .min_scrolled_height(0.0)
        .max_width(size.x)
        .max_height(size.y)
        .show(&mut child, content);
}
