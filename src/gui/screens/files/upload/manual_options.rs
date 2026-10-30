//! Policy fields of a manual one-off upload (shard size, retries, placement,
//! K+M), edited directly in the saved settings.

use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use eframe::egui;

/// Draws the manual policy grid and the resulting parity overhead. Called by
/// `advanced::show` in manual target mode.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tr("Manual upload policy")).strong());
    egui::Grid::new("upload-manual-options")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label(tr("Shard size (MiB)"));
            ui.add(
                egui::DragValue::new(&mut state.settings.shard_mib)
                    .range(1..=crate::config::constants::MAX_SHARD_MIB),
            );
            ui.end_row();

            ui.label(tr("Retries"));
            ui.add(egui::DragValue::new(&mut state.settings.retries).range(0..=100));
            ui.end_row();

            ui.label(tr("Placement"));
            crate::gui::widgets::placement_picker(
                ui,
                "upload-placement",
                &mut state.settings.placement,
            );
            ui.end_row();

            ui.label(tr("Data shards (K)"));
            ui.add(egui::DragValue::new(&mut state.settings.data_shards).range(1..=255));
            ui.end_row();

            ui.label(tr("Parity shards (M)"));
            ui.add(egui::DragValue::new(&mut state.settings.parity_shards).range(0..=254));
            ui.end_row();
        });
    if let Some(note) = state.settings.placement.protection_note() {
        ui.small(crate::gui::i18n::tr(note));
    }

    if state.settings.parity_shards > 0 && state.settings.data_shards > 0 {
        let overhead =
            state.settings.parity_shards as f64 / state.settings.data_shards as f64 * 100.0;
        ui.label(trf(
            "Erasure coding: {k}+{m} · parity overhead {pct}%",
            &[
                ("k", &state.settings.data_shards),
                ("m", &state.settings.parity_shards),
                ("pct", &format!("{overhead:.1}")),
            ],
        ));
    } else {
        ui.label(tr("Erasure coding disabled (M = 0)."));
    }
}
