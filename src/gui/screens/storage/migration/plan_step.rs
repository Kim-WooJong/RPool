//! Step 1: pick the pool, probe depth and speeds, then create the plan
//! (`pool migrate plan`) in a background thread.
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState, _task: &mut TaskRunner) {
    let planning = state.migration.planning();
    ui.add_enabled_ui(!planning, |ui| {
        egui::Grid::new("migration-plan-grid")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Pool");
                let mut chosen = state.migration.pool.clone();
                egui::ComboBox::from_id_salt("migration-pool")
                    .selected_text(if chosen.is_empty() {
                        "Select pool"
                    } else {
                        chosen.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for name in &state.pool_names {
                            ui.selectable_value(&mut chosen, name.clone(), name.as_str());
                        }
                    });
                if chosen != state.migration.pool {
                    state.migration.select_pool(&chosen);
                }
                ui.end_row();

                ui.label("Check shards");
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut state.migration.probe_full, false, "Quick (sizes)");
                    ui.radio_value(&mut state.migration.probe_full, true, "Full (hash)")
                        .on_hover_text("Reads and hashes every shard of affected archives. Slower, finds corrupt shards.");
                });
                ui.end_row();

                ui.label("Speed");
                ui.checkbox(&mut state.migration.measure_speed, "Measure speed")
                    .on_hover_text("Writes, reads back and deletes one 8 MiB test object per account under .rpool-sync/bench/.");
                ui.end_row();

                ui.label("Download MiB/s");
                ui.add(
                    egui::DragValue::new(&mut state.migration.download_mib_s)
                        .range(0.0..=100_000.0)
                        .speed(0.5),
                );
                ui.end_row();
                ui.label("Upload MiB/s");
                ui.add(
                    egui::DragValue::new(&mut state.migration.upload_mib_s)
                        .range(0.0..=100_000.0)
                        .speed(0.5),
                );
                ui.end_row();
            });
    });
    theme::hint(
        ui,
        if state.migration.measure_speed {
            "Measured speeds replace the fields; the fields are used when a measurement fails. 0 = unknown."
        } else {
            "0 = unknown: the plan then shows no time estimate."
        },
    );
    ui.horizontal_wrapped(|ui| {
        let can = !planning && !state.migration.pool.is_empty();
        if theme::primary_button(ui, can, "Create plan")
            .on_hover_text(
                "Read-only check of every archive, then the plan is saved in the cloud journal.",
            )
            .clicked()
        {
            let remotes = state
                .pool_definitions
                .get(&state.migration.pool)
                .map(|p| p.remotes.clone())
                .unwrap_or_default();
            state
                .migration
                .start_plan(&state.settings.rclone, remotes, state.settings.workers);
        }
        if planning {
            ui.spinner();
            ui.label(if state.migration.measure_speed {
                "Measuring speed and planning… this can take a while."
            } else {
                "Planning… this can take a while."
            });
        }
    });
    if state.pool_names.is_empty() {
        theme::hint(
            ui,
            "No pool is saved yet; create one under Storage › Pools.",
        );
    }
}
