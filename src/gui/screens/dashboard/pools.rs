use crate::gui::screens::dashboard::health::{pool_health, PoolHealth};
use crate::gui::screens::dashboard::DashboardPool;
use crate::gui::state::{GuiState, Page, StorageSection};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Pools")
                .size(theme::SECTION_TITLE_SIZE)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link("Open Storage").clicked() {
                state.page = Page::Storage;
                state.storage_section = StorageSection::Pools;
            }
        });
    });
    ui.add_space(theme::SUBSECTION_GAP);

    if state.dashboard.pools.is_empty() {
        ui.label("No pools are configured.");
        return;
    }

    egui::Grid::new("dashboard-pools")
        .num_columns(4)
        .striped(true)
        .spacing([18.0, 7.0])
        .show(ui, |ui| {
            ui.strong("Pool");
            ui.strong("Coding");
            ui.strong("Providers");
            ui.strong("Status");
            ui.end_row();

            for pool in &state.dashboard.pools {
                pool_row(ui, pool, &state.crypt_remotes);
            }
        });
}

fn pool_row(ui: &mut egui::Ui, pool: &DashboardPool, crypt_remotes: &[String]) {
    ui.label(&pool.name);
    ui.monospace(format!("{}+{}", pool.data_shards, pool.parity_shards));
    ui.monospace(pool.remotes.len().to_string());

    match pool_health(pool, crypt_remotes) {
        PoolHealth::Ready => status_badge(ui, "Ready", StatusTone::Success),
        PoolHealth::Checking => status_badge(ui, "Checking", StatusTone::Neutral),
        PoolHealth::Empty => status_badge(ui, "Empty", StatusTone::Error),
        PoolHealth::MissingRemote => status_badge(ui, "Missing remote", StatusTone::Warning),
    }
    ui.end_row();
}
