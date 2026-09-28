use crate::gui::settings;
use crate::gui::state::GuiState;
use crate::models::Placement;
use crate::remote_root::{load_remote_root_store, remove_remote_root, set_remote_root};
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.heading("GUI / operation defaults");
    ui.label("These values are shared by the GUI screens. Saving is explicit so temporary changes remain temporary until requested.");
    ui.separator();

    egui::Grid::new("gui-settings")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            ui.label("rclone executable");
            ui.add(egui::TextEdit::singleline(&mut state.settings.rclone).desired_width(360.0));
            ui.end_row();

            ui.label("Global crypt folder fallback");
            ui.add(
                egui::TextEdit::singleline(&mut state.settings.default_remote_path)
                    .hint_text("rpool")
                    .desired_width(360.0),
            );
            ui.end_row();

            ui.label("Workers");
            ui.add(egui::DragValue::new(&mut state.settings.workers).range(1..=256));
            ui.end_row();

            ui.label("Retries");
            ui.add(egui::DragValue::new(&mut state.settings.retries).range(0..=100));
            ui.end_row();

            ui.label("Shard size (MiB)");
            ui.add(egui::DragValue::new(&mut state.settings.shard_mib).range(1..=1024 * 1024));
            ui.end_row();

            ui.label("Data shards (K)");
            ui.add(egui::DragValue::new(&mut state.settings.data_shards).range(1..=255));
            ui.end_row();

            ui.label("Parity shards (M)");
            ui.add(egui::DragValue::new(&mut state.settings.parity_shards).range(0..=254));
            ui.end_row();

            ui.label("Placement");
            egui::ComboBox::from_id_salt("settings-placement")
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
                });
            ui.end_row();
        });

    ui.add_space(12.0);
    if ui.button("Save GUI defaults").clicked() {
        state.settings_notice = Some(match settings::save(&state.settings) {
            Ok(path) => format!("Saved to {}", path.display()),
            Err(error) => error,
        });
    }
    if let Some(notice) = &state.settings_notice {
        ui.label(notice);
    }

    ui.separator();
    show_remote_roots(ui, state);

    ui.separator();
    ui.label("Selected upload destinations");
    if state.settings.remotes.is_empty() {
        ui.label("No saved upload destinations.");
    } else {
        for remote in &state.settings.remotes {
            ui.monospace(remote);
        }
    }
}

fn show_remote_roots(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.heading("Per-remote default paths");
    ui.label("Use this when the root of a remote is not the correct storage/filesystem location. Explicit paths always override this default.");
    ui.small("Example: Instance → /data/crypt makes capacity checks use Instance:/data/crypt instead of Instance:.");

    let rows: Vec<(String, String)> = state
        .remote_roots
        .iter()
        .map(|(name, path)| (name.clone(), path.clone()))
        .collect();
    let mut remove = None;
    egui::Grid::new("remote-root-list")
        .num_columns(3)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.strong("Remote");
            ui.strong("Default path");
            ui.label("");
            ui.end_row();
            for (name, path) in rows {
                ui.monospace(format!("{name}:"));
                ui.monospace(path);
                if ui.small_button("Remove").clicked() {
                    remove = Some(name);
                }
                ui.end_row();
            }
        });

    if let Some(name) = remove {
        let notice = match remove_remote_root(&name) {
            Ok(path) => {
                reload_remote_roots(state);
                format!("Removed {name}: default path from {}", path.display())
            }
            Err(error) => format!("Failed to remove remote default path: {error:#}"),
        };
        state.settings_notice = Some(notice);
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.remote_root_name)
                .hint_text("Instance")
                .desired_width(180.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut state.remote_root_path)
                .hint_text("/data/crypt")
                .desired_width(260.0),
        );
        if ui.button("Set / replace").clicked() {
            let name = state.remote_root_name.trim().to_string();
            let path = state.remote_root_path.trim().to_string();
            if name.is_empty() || path.is_empty() {
                state.settings_notice = Some("Enter both a remote name and default path.".to_string());
            } else {
                let notice = match set_remote_root(&name, &path) {
                    Ok(config) => {
                        reload_remote_roots(state);
                        state.remote_root_name.clear();
                        state.remote_root_path.clear();
                        format!("Saved {name}: → {path} in {}", config.display())
                    }
                    Err(error) => format!("Failed to save remote default path: {error:#}"),
                };
                state.settings_notice = Some(notice);
            }
        }
    });
}

fn reload_remote_roots(state: &mut GuiState) {
    match load_remote_root_store() {
        Ok(store) => state.remote_roots = store.roots,
        Err(error) => state.settings_notice = Some(format!("Failed to reload remote default paths: {error:#}")),
    }
}
