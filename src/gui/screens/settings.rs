//! Settings page: a tab strip over General, New-pool defaults, Encryption &
//! paths, Network and Portable configuration. Rendered by `gui::app` for
//! `Page::Settings`; edits live in `GuiState::settings` until saved.
use crate::gui::i18n::{tr, trf};
use crate::gui::settings;
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::models::Placement;
use eframe::egui;

/// Tab id of General (language, rclone path, retries, diagnostics export).
const GENERAL: u8 = 0;
/// Tab id of New-pool defaults (shard size, K/M, placement, saved destinations).
const POOL_DEFAULTS: u8 = 3;
/// Tab id of Encryption & paths (crypt defaults and per-remote default paths).
const ENCRYPTION: u8 = 1;
/// Tab id of Portable configuration (`portable_config`).
const PORTABLE: u8 = 2;
/// Tab id of Network (`network_settings`).
const NETWORK: u8 = 4;

/// Renders the Settings page; the selected tab is kept in egui temp data
/// (`settings-tab`), so it resets to General on restart.
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
            (GENERAL, tr("General")),
            (POOL_DEFAULTS, tr("New-pool defaults")),
            (ENCRYPTION, tr("Encryption & paths")),
            (NETWORK, tr("Network")),
            (PORTABLE, tr("Portable configuration")),
        ],
    );
    ui.ctx().data_mut(|data| data.insert_temp(tab_id, tab));
    theme::page_body(ui, "settings", |ui| match tab {
        ENCRYPTION => {
            theme::two_up(ui, state, show_encryption, |ui, state| {
                theme::card_section(ui, tr("Remote default paths"), Some(tr("Where RPool looks on a provider when the remote root is not the right place.")), |_| {}, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr("Global crypt folder fallback"));
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.default_remote_path)
                                .hint_text("rpool")
                                .desired_width(ui.available_width().min(240.0)),
                        );
                    });
                    theme::hint(ui, tr("Each provider's own folder is set on its card in Storage › Providers (Location › Change…); encrypted providers follow it there."));
                    save_row(ui, state);
                });
            });
        }
        PORTABLE => super::portable_config::show(ui, state, task),
        NETWORK => super::network_settings::show(ui, state),
        POOL_DEFAULTS => pool_defaults(ui, state),
        _ => {
            general(ui, state);
            super::diagnostics_export::show(ui, state, task);
        }
    });
}

/// Save settings button plus the last save notice, shared by several cards.
fn save_row(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.horizontal_wrapped(|ui| {
        if theme::primary_button(ui, true, tr("Save settings")).clicked() {
            state.settings_notice = Some(match settings::save(&state.settings) {
                Ok(path) => trf("Saved to {path}", &[("path", &path.display())]),
                Err(error) => error,
            });
        }
        if let Some(notice) = &state.settings_notice {
            ui.label(notice);
        }
    });
}

/// General card: language (applied immediately), rclone executable and retries.
fn general(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(
        ui,
        tr("General"),
        Some(tr(
            "Used by every screen. Changes stay temporary until saved.",
        )),
        |_| {},
        |ui| {
            egui::Grid::new("gui-settings")
                .num_columns(2)
                .spacing([18.0, 10.0])
                .show(ui, |ui| {
                    ui.label(tr("Language"));
                    let mut language = state.settings.language;
                    egui::ComboBox::from_id_salt("settings-language")
                        .selected_text(language.native_name())
                        .show_ui(ui, |ui| {
                            for option in crate::gui::i18n::Language::ALL {
                                ui.selectable_value(&mut language, option, option.native_name());
                            }
                        });
                    if language != state.settings.language {
                        // Applies at once; saved with the other settings.
                        state.settings.language = language;
                        crate::gui::i18n::set_language(language);
                        crate::gui::i18n::fonts::install(ui.ctx(), language);
                    }
                    ui.end_row();
                    ui.label(tr("rclone executable"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.settings.rclone)
                            .desired_width((ui.available_width() - 20.0).clamp(160.0, 360.0)),
                    );
                    ui.end_row();
                    ui.label(tr("Retries"));
                    ui.add(egui::DragValue::new(&mut state.settings.retries).range(0..=100));
                    ui.end_row();
                });
            save_row(ui, state);
        },
    );
}

/// New-pool defaults: shard size, data/parity counts and placement, plus the
/// read-only list of saved upload destinations.
fn pool_defaults(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::two_up(
        ui,
        state,
        |ui, state| {
            theme::card_section(ui, tr("Defaults for new pools"), Some(tr("Pre-filled when you create a pool or upload without one. Existing pools keep their own policy.")), |_| {}, |ui| {
            egui::Grid::new("gui-pool-defaults").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
                ui.label(tr("Shard size (MiB)"));
                ui.add(egui::DragValue::new(&mut state.settings.shard_mib).range(1..=crate::config::constants::MAX_SHARD_MIB));
                ui.end_row();
                ui.label(tr("Data shards (K)"));
                ui.add(egui::DragValue::new(&mut state.settings.data_shards).range(1..=255));
                ui.end_row();
                ui.label(tr("Parity shards (M)"));
                ui.add(egui::DragValue::new(&mut state.settings.parity_shards).range(0..=254));
                ui.end_row();
                ui.label(tr("Placement"));
                egui::ComboBox::from_id_salt("settings-placement")
                    .selected_text(tr(state.settings.placement.label()))
                    .show_ui(ui, |ui| {
                        for placement in [Placement::RoundRobin, Placement::FreeRatio, Placement::Proportional, Placement::Resilient, Placement::CapacityFirst] {
                            ui.selectable_value(&mut state.settings.placement, placement, tr(placement.label()));
                        }
                    });
                ui.end_row();
            });
            if let Some(note) = state.settings.placement.protection_note() {
                theme::hint(ui, tr(note));
            }
            save_row(ui, state);
        });
        },
        |ui, state| {
            theme::card_section(
                ui,
                tr("Saved upload destinations"),
                Some(tr("Used by Upload in manual mode.")),
                |_| {},
                |ui| {
                    if state.settings.remotes.is_empty() {
                        theme::hint(ui, tr("No saved upload destinations."));
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

/// Card wrapper around `encryption_body`.
fn show_encryption(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(ui, tr("Encryption for new providers"), Some(tr("Used when RPool creates a crypt remote. Existing keys and encrypted folders are never changed or rotated.")), |_| {}, |ui| encryption_body(ui, state));
}

/// Crypt defaults for new providers (entropy, filename/directory encryption,
/// name encoding); validated before the save button is enabled.
fn encryption_body(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::hint(
        ui,
        tr("Password entropy controls generated password randomness, not rclone’s cipher key size."),
    );
    let defaults = &mut state.settings.encryption;
    egui::Grid::new("encryption-defaults")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            ui.label(tr("Password entropy"));
            egui::ComboBox::from_id_salt("encryption-default-entropy")
                .selected_text(trf("{bits} bits", &[("bits", &defaults.entropy_bits)]))
                .show_ui(ui, |ui| {
                    for bits in [128, 256, 512, 1024] {
                        ui.selectable_value(
                            &mut defaults.entropy_bits,
                            bits,
                            trf("{bits} bits", &[("bits", &bits)]),
                        );
                    }
                });
            ui.end_row();
            ui.label(tr("Filename encryption"));
            egui::ComboBox::from_id_salt("encryption-default-filenames")
                .selected_text(&defaults.filename_encryption)
                .show_ui(ui, |ui| {
                    for mode in ["standard", "obfuscate", "off"] {
                        ui.selectable_value(&mut defaults.filename_encryption, mode.into(), mode);
                    }
                });
            ui.end_row();
            ui.label(tr("Directory name encryption"));
            ui.checkbox(&mut defaults.directory_encryption, tr("Enabled"));
            ui.end_row();
            ui.label(tr("Name encoding"));
            ui.vertical(|ui| {
                let mut index = crate::gui::screens::storage::providers::encoding_index(
                    &defaults.filename_encoding,
                );
                crate::gui::screens::storage::providers::encoding_combo(
                    ui,
                    "encryption-default-encoding",
                    &mut index,
                );
                defaults.filename_encoding =
                    crate::config_sync::provision::FILENAME_ENCODINGS[index].into();
            });
            ui.end_row();
        });
    theme::hint(ui, tr("New crypt uses the provider's remote default path exactly (Remote default paths, next to this card). No extra folder is added; existing crypt paths never move."));
    let validation = defaults.validate();
    if let Err(error) = &validation {
        ui.colored_label(egui::Color32::LIGHT_RED, error.to_string());
    }
    if ui
        .add_enabled(
            validation.is_ok(),
            egui::Button::new(tr("Save encryption defaults")),
        )
        .clicked()
    {
        state.settings_notice = Some(match settings::save(&state.settings) {
            Ok(path) => trf("Saved to {path}", &[("path", &path.display())]),
            Err(error) => error,
        });
    }
    if let Some(notice) = &state.settings_notice {
        ui.label(notice);
    }
}
