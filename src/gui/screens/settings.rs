use crate::gui::settings;
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::models::Placement;
use crate::remote_root::{load_remote_root_store, remove_remote_root, set_remote_root};
use eframe::egui;

const GENERAL: u8 = 0;
const POOL_DEFAULTS: u8 = 3;
const ENCRYPTION: u8 = 1;
const PORTABLE: u8 = 2;

pub(crate) fn show(
    ui: &mut egui::Ui,
    state: &mut GuiState,
    task: &mut crate::gui::task::TaskRunner,
) {
    let tab_id = egui::Id::new("settings-tab");
    let mut tab = ui
        .ctx()
        .data_mut(|data| data.get_temp::<u8>(tab_id).unwrap_or(GENERAL));
    theme::tabs(
        ui,
        &mut tab,
        &[
            (GENERAL, "General"),
            (POOL_DEFAULTS, "New-pool defaults"),
            (ENCRYPTION, "Encryption & paths"),
            (PORTABLE, "Portable configuration"),
        ],
    );
    ui.ctx().data_mut(|data| data.insert_temp(tab_id, tab));
    theme::page_body(ui, "settings", |ui| match tab {
        ENCRYPTION => {
            theme::two_up(ui, state, show_encryption, |ui, state| {
                theme::card_section(ui, "Remote default paths", Some("Where RPool looks on a provider when the remote root is not the right place."), |_| {}, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Global crypt folder fallback");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.default_remote_path)
                                .hint_text("rpool")
                                .desired_width(ui.available_width().min(240.0)),
                        );
                    });
                    show_remote_roots(ui, state);
                    save_row(ui, state);
                });
            });
        }
        PORTABLE => super::portable_config::show(ui, state, task),
        POOL_DEFAULTS => pool_defaults(ui, state),
        _ => general(ui, state),
    });
}

fn save_row(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.horizontal_wrapped(|ui| {
        if theme::primary_button(ui, true, "Save settings").clicked() {
            state.settings_notice = Some(match settings::save(&state.settings) {
                Ok(path) => format!("Saved to {}", path.display()),
                Err(error) => error,
            });
        }
        if let Some(notice) = &state.settings_notice {
            ui.label(notice);
        }
    });
}

fn general(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(
        ui,
        "General",
        Some("Used by every screen. Changes stay temporary until saved."),
        |_| {},
        |ui| {
            egui::Grid::new("gui-settings").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
            ui.label("rclone executable");
            ui.add(egui::TextEdit::singleline(&mut state.settings.rclone).desired_width((ui.available_width() - 20.0).clamp(160.0, 360.0)));
            ui.end_row();
            ui.label("Workers").on_hover_text("Parallel transfers for restore, verify, scrub, repair, drain and health checks.");
            ui.add(egui::DragValue::new(&mut state.settings.workers).range(1..=256));
            ui.end_row();
            ui.label("Retries");
            ui.add(egui::DragValue::new(&mut state.settings.retries).range(0..=100));
            ui.end_row();
        });
            save_row(ui, state);
        },
    );
}

fn pool_defaults(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::two_up(
        ui,
        state,
        |ui, state| {
            theme::card_section(ui, "Defaults for new pools", Some("Pre-filled when you create a pool or upload without one. Existing pools keep their own policy."), |_| {}, |ui| {
            egui::Grid::new("gui-pool-defaults").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
                ui.label("Shard size (MiB)");
                ui.add(egui::DragValue::new(&mut state.settings.shard_mib).range(1..=crate::config::constants::MAX_SHARD_MIB));
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
                        for placement in [Placement::RoundRobin, Placement::FreeRatio, Placement::Resilient, Placement::CapacityFirst] {
                            ui.selectable_value(&mut state.settings.placement, placement, placement.label());
                        }
                    });
                ui.end_row();
            });
            if let Some(note) = state.settings.placement.protection_note() {
                theme::hint(ui, note);
            }
            save_row(ui, state);
        });
        },
        |ui, state| {
            theme::card_section(
                ui,
                "Saved upload destinations",
                Some("Used by Upload in manual mode."),
                |_| {},
                |ui| {
                    if state.settings.remotes.is_empty() {
                        theme::hint(ui, "No saved upload destinations.");
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("settings-destinations")
                            .max_height(240.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for remote in &state.settings.remotes {
                                    ui.monospace(remote);
                                }
                            });
                    }
                },
            );
        },
    );
}

fn show_remote_roots(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(egui::RichText::new("Per-remote default paths").strong());
    theme::hint(ui, "Use this when the root of a remote is not the correct storage location. Explicit paths always override this default.");
    theme::hint(ui, "Example: Instance › /data/crypt makes capacity checks use Instance:/data/crypt instead of Instance:.");

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
    ui.horizontal_wrapped(|ui| {
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
                state.settings_notice =
                    Some("Enter both a remote name and default path.".to_string());
            } else {
                let notice = match set_remote_root(&name, &path) {
                    Ok(config) => {
                        reload_remote_roots(state);
                        state.remote_root_name.clear();
                        state.remote_root_path.clear();
                        format!("Saved {name}: › {path} in {}", config.display())
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
        Err(error) => {
            state.settings_notice =
                Some(format!("Failed to reload remote default paths: {error:#}"))
        }
    }
}

fn show_encryption(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(ui, "Encryption for new providers", Some("Used when RPool creates a crypt remote. Existing keys and encrypted folders are never changed or rotated."), |_| {}, |ui| encryption_body(ui, state));
}

fn encryption_body(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::hint(
        ui,
        "Password entropy controls generated password randomness, not rclone’s cipher key size.",
    );
    let defaults = &mut state.settings.encryption;
    egui::Grid::new("encryption-defaults")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            ui.label("Password entropy");
            egui::ComboBox::from_id_salt("encryption-default-entropy")
                .selected_text(format!("{} bits", defaults.entropy_bits))
                .show_ui(ui, |ui| {
                    for bits in [128, 256, 512, 1024] {
                        ui.selectable_value(
                            &mut defaults.entropy_bits,
                            bits,
                            format!("{bits} bits"),
                        );
                    }
                });
            ui.end_row();
            ui.label("Filename encryption");
            egui::ComboBox::from_id_salt("encryption-default-filenames")
                .selected_text(&defaults.filename_encryption)
                .show_ui(ui, |ui| {
                    for mode in ["standard", "obfuscate", "off"] {
                        ui.selectable_value(&mut defaults.filename_encryption, mode.into(), mode);
                    }
                });
            ui.end_row();
            ui.label("Directory name encryption");
            ui.checkbox(&mut defaults.directory_encryption, "Enabled");
            ui.end_row();
        });
    theme::hint(ui, "New crypt uses the provider's remote default path exactly (Remote default paths, next to this card). No extra folder is added; existing crypt paths never move.");
    let validation = defaults.validate();
    if let Err(error) = &validation {
        ui.colored_label(egui::Color32::LIGHT_RED, error.to_string());
    }
    if ui
        .add_enabled(
            validation.is_ok(),
            egui::Button::new("Save encryption defaults"),
        )
        .clicked()
    {
        state.settings_notice = Some(match settings::save(&state.settings) {
            Ok(path) => format!("Saved to {}", path.display()),
            Err(error) => error,
        });
    }
    if let Some(notice) = &state.settings_notice {
        ui.label(notice);
    }
}
