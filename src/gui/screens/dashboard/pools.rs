//! Pools pane of the overview page.
use crate::gui::i18n::tr;
use crate::gui::screens::dashboard::health::{pool_health, PoolHealth};
use crate::gui::screens::dashboard::DashboardPool;
use crate::gui::state::{GuiState, Page, StorageSection};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;

/// Draw the pools table (name, coding, provider count, readiness) with a
/// link to the Storage page.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(tr("Pools"))
                .size(theme::SECTION_TITLE_SIZE)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.link(tr("Open Storage")).clicked() {
                state.page = Page::Storage;
                state.storage_section = StorageSection::Pools;
            }
        });
    });
    ui.add_space(theme::SUBSECTION_GAP);

    if state.dashboard.pools.is_empty() {
        ui.label(tr("No pools are configured."));
        return;
    }

    egui::Grid::new("dashboard-pools")
        .num_columns(4)
        .striped(true)
        .spacing([18.0, 7.0])
        .show(ui, |ui| {
            ui.strong(tr("Pool"));
            ui.strong(tr("Coding"));
            ui.strong(tr("Providers"));
            ui.strong(tr("Status"));
            ui.end_row();

            for pool in &state.dashboard.pools {
                pool_row(ui, pool, &state.crypt_remotes);
            }
        });
}

/// One pool row with its readiness badge.
fn pool_row(ui: &mut egui::Ui, pool: &DashboardPool, crypt_remotes: &[String]) {
    ui.label(&pool.name);
    ui.monospace(format!("{}+{}", pool.data_shards, pool.parity_shards));
    ui.monospace(pool.remotes.len().to_string());

    match pool_health(pool, crypt_remotes) {
        PoolHealth::Ready => status_badge(ui, tr("Ready"), StatusTone::Success),
        PoolHealth::Checking => status_badge(ui, tr("Checking"), StatusTone::Neutral),
        PoolHealth::Empty => status_badge(ui, tr("Empty"), StatusTone::Error),
        PoolHealth::MissingRemote => status_badge(ui, tr("Missing remote"), StatusTone::Warning),
    }
    ui.end_row();
}
