use crate::gui::state::GuiState;
use crate::models::Placement;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Manual upload policy").strong());
    egui::Grid::new("upload-manual-options")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Shard size (MiB)");
            ui.add(egui::DragValue::new(&mut state.settings.shard_mib).range(1..=1024 * 1024));
            ui.end_row();

            ui.label("Workers");
            ui.add(egui::DragValue::new(&mut state.settings.workers).range(1..=256));
            ui.end_row();

            ui.label("Retries");
            ui.add(egui::DragValue::new(&mut state.settings.retries).range(0..=100));
            ui.end_row();

            ui.label("Placement");
            egui::ComboBox::from_id_salt("upload-placement")
                .selected_text(state.settings.placement.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.settings.placement,
                        Placement::RoundRobin,
                        Placement::RoundRobin.label(),
                    );
                    ui.selectable_value(
                        &mut state.settings.placement,
                        Placement::FreeRatio,
                        Placement::FreeRatio.label(),
                    );
                    ui.selectable_value(
                        &mut state.settings.placement,
                        Placement::Resilient,
                        Placement::Resilient.label(),
                    );
                });
            ui.end_row();

            ui.label("Data shards (K)");
            ui.add(egui::DragValue::new(&mut state.settings.data_shards).range(1..=255));
            ui.end_row();

            ui.label("Parity shards (M)");
            ui.add(egui::DragValue::new(&mut state.settings.parity_shards).range(0..=254));
            ui.end_row();
        });

    if state.settings.parity_shards > 0 && state.settings.data_shards > 0 {
        let overhead =
            state.settings.parity_shards as f64 / state.settings.data_shards as f64 * 100.0;
        ui.label(format!(
            "Erasure coding: {}+{} · parity overhead {:.1}%",
            state.settings.data_shards, state.settings.parity_shards, overhead
        ));
    } else {
        ui.label("Erasure coding disabled (M = 0).");
    }
}
