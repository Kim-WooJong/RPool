//! Storage › Providers: connect cloud accounts (rclone config wizard), set
//! up crypt remotes automatically or by hand, check provider health, and the
//! provider cards with usage, limits, keep-alive and name encoding. Also
//! hosts the drain card shown under Account changes.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{local_file_field, output_file_field};
use eframe::egui;
use std::ffi::OsString;

mod card;
mod grid;
pub(crate) mod limits_cache;
mod limits_dialog;
mod limits_model;
mod model;
mod name_encoding;

pub(crate) use limits_model::wait as wait_text;
pub(crate) use name_encoding::{combo as encoding_combo, index_of as encoding_index};
#[cfg(any(test, debug_assertions))]
pub(crate) mod sample;

/// State of the Providers tab, held in `GuiState::providers`.
#[derive(Debug, Default)]
pub(crate) struct ProviderForm {
    /// External rclone config wizard in progress; buttons stay disabled until it closes.
    pub(crate) connection: Option<crate::provider::onboarding::ConnectionSetup>,
    /// Base providers that still lack a crypt remote (from the usage refresh in `gui::app`).
    pub(crate) missing_encryption: Vec<String>,
    /// Whether `missing_encryption` comes from a completed discovery.
    pub(crate) discovery_known: bool,
    /// The last automatic encryption run failed or could not start.
    pub(crate) encryption_failed: bool,
    /// A provider refresh was requested; `gui::app` takes and handles it.
    pub(crate) refresh_requested: bool,
    /// The "New encrypted provider" dialog is open.
    pub(crate) setup_open: bool,
    /// Name of the crypt remote to create (`--name`).
    pub(crate) crypt_name: String,
    /// Non-crypt remote the new crypt wraps (`--provider`).
    pub(crate) backing_provider: String,
    /// Index into the entropy choices 256 / 128 / 512 / 1024 bits.
    pub(crate) entropy_index: usize,
    /// Index into the filename encryption modes standard / obfuscate / off.
    pub(crate) filename_index: usize,
    /// Index into `FILENAME_ENCODINGS`.
    pub(crate) encoding_index: usize,
    /// Leave directory names unencrypted (`--directory-encryption=false`).
    pub(crate) disable_directory_encryption: bool,
    /// "I understand that losing these keys…" confirmation; enables key generation.
    pub(crate) backup_acknowledged: bool,
    /// Result of the last connect / encryption action, shown in the connect card and dialog.
    pub(crate) setup_notice: Option<String>,
    /// Health check scope: a pool name, or empty for crypt remotes (`--pool`).
    pub(crate) health_pool: String,
    /// Specific crypt remotes to check when no pool is chosen (`--remote`).
    pub(crate) health_remotes: Vec<String>,
    /// Drain: manifest file or remote path.
    pub(crate) manifest: String,
    /// Drain: crypt remote to move shards off (`--from`).
    pub(crate) from: String,
    /// Drain: crypt remote to move shards to (`--to`).
    pub(crate) to: String,
    /// Drain: optional output manifest (`--output`).
    pub(crate) output: String,
    /// Drain: preview only (`--dry-run`).
    pub(crate) dry_run: bool,
    /// Drain: delete old shards after verification (`--delete-source`).
    pub(crate) delete_source: bool,
    /// Drain: allow weaker failure-domain safety (`--allow-risky`).
    pub(crate) allow_risky: bool,
    /// Start error of the last health check or drain.
    pub(crate) error: Option<String>,
    /// Account limits shown on the cards.
    pub(crate) limits: limits_cache::LimitsCache,
    /// State of the per-account "Limits…" dialog.
    pub(crate) limits_editor: limits_dialog::LimitsEditor,
    /// State of the "Name encoding" dialog.
    pub(crate) name_encoding: name_encoding::Editor,
}

impl ProviderForm {
    /// Resets the encryption dialog choices to the Settings › Encryption defaults.
    fn apply_defaults(&mut self, defaults: &crate::config_sync::provision::EncryptionDefaults) {
        self.entropy_index = [256, 128, 512, 1024]
            .iter()
            .position(|v| *v == defaults.entropy_bits)
            .unwrap_or(3);
        self.filename_index = ["standard", "obfuscate", "off"]
            .iter()
            .position(|v| *v == defaults.filename_encryption)
            .unwrap_or(0);
        self.encoding_index = name_encoding::index_of(&defaults.filename_encoding);
        self.disable_directory_encryption = !defaults.directory_encryption;
        self.backup_acknowledged = false;
    }
}

/// Encryption status text of a provider card (unknown, configured, running,
/// failed or pending); used by `model::cards`.
fn encryption_status(known: bool, missing: bool, running: bool, failed: bool) -> &'static str {
    if !known {
        tr("Encryption status unknown")
    } else if !missing {
        tr("Encryption configured")
    } else if running {
        tr("Setting up encryption…")
    } else if failed {
        tr("Setup incomplete — retry")
    } else {
        tr("Encryption setup pending")
    }
}

/// Starts `rpool provider ensure-encryption` with the encryption defaults,
/// creating crypt remotes for base providers that lack one. Called by
/// `gui::app` after discovery and by "Retry automatic encryption".
pub(crate) fn start_automatic_encryption(
    state: &GuiState,
    task: &mut TaskRunner,
) -> Result<(), String> {
    state
        .settings
        .encryption
        .validate()
        .map_err(|e| e.to_string())?;
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("ensure-encryption"),
    ];
    args.extend(state.settings.encryption.ensure_args());
    task.start_rpool(
        crate::gui::app::AUTO_ENCRYPTION_TASK,
        &state.settings.rclone,
        args,
    )
}

/// Renders the Providers tab (`storage::show`) and its dialogs; refreshes
/// the limits cache when due.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    limits_cache::refresh(state);
    theme::page_body(ui, "providers", |ui| {
        theme::page_header(
            ui,
            tr("Providers"),
            Some(tr(
                "Connect cloud accounts, keep them encrypted, and check that they answer.",
            )),
        );
        theme::two_up(
            ui,
            &mut (&mut *state, &mut *task),
            |ui, s| connect_card(ui, s.0, s.1),
            |ui, s| health_card(ui, s.0, s.1),
        );
        list_card(ui, state, task);
        super::speed_test::providers_card(ui, state, task);
    });
    encryption_dialog(ui.ctx(), state, task);
    name_encoding::show(
        ui.ctx(),
        &mut state.providers.name_encoding,
        &state.settings.rclone,
        task,
    );
    if limits_dialog::show(ui.ctx(), &mut state.providers.limits_editor) {
        state.providers.limits.invalidate();
    }
}

/// "Connect & encrypt" card: opens the rclone wizard, the manual encryption
/// dialog, or retries automatic encryption.
fn connect_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let idle = !task.is_running() && state.providers.connection.is_none();
    theme::card_section(
        ui,
        tr("Connect & encrypt"),
        Some(tr(
            "Encryption is set up automatically for connected base providers.",
        )),
        |_| {},
        |ui| {
            ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, idle, tr("+ Connect cloud provider")).clicked() {
                state.providers.setup_notice = Some(match crate::provider::onboarding::open_setup(&state.settings.rclone) {
                    Ok(connection) => {
                        state.providers.connection = Some(connection);
                        tr("rclone setup opened in your terminal. Complete the login, then quit the wizard. Providers will refresh automatically when the wizard closes.").into()
                    },
                    Err(error) => error.to_string(),
                });
            }
            if ui.add_enabled(idle, egui::Button::new(tr("Set up encryption…"))).clicked() {
                state.providers.apply_defaults(&state.settings.encryption);
                state.providers.setup_open = true;
            }
            if ui.add_enabled(idle, egui::Button::new(tr("Retry automatic encryption"))).clicked() {
                state.providers.encryption_failed = false;
                state.providers.setup_notice = Some(match start_automatic_encryption(state, task) {
                    Ok(()) => tr("Checking encryption for connected providers. See Activity for progress.").into(),
                    Err(error) => error,
                });
            }
        });
            if state.providers.connection.is_some() {
                ui.horizontal_wrapped(|ui| {
                ui.spinner();
                ui.label(tr("Waiting for the connection wizard to close…"));
                if ui.small_button(tr("Wizard already closed — refresh")).on_hover_text(tr("Use only after closing the external rclone wizard if completion was not detected.")).clicked() {
                    state.providers.connection = None;
                    state.providers.refresh_requested = true;
                }
            });
            }
            if let Some(notice) = &state.providers.setup_notice {
                ui.label(notice);
            }
            theme::hint(ui, tr("Back up your rclone configuration (Settings › Portable configuration) to preserve generated keys. Defaults for new keys are in Settings › Encryption."));
        },
    );
}

/// "Connected providers" card: provider cards in a responsive grid, and the
/// handling of a card's button (encryption, limits, name encoding, keep-alive).
fn list_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    // A pool as the health monitor's scope also narrows the list to its providers.
    let count = model::pool_scope(state).map_or(state.backing_remotes.len(), |s| s.len());
    let scoped = !state.providers.health_pool.trim().is_empty();
    let mut refresh = false;
    let title = if scoped {
        trf(
            "Connected providers in pool {pool} · {count} of {all}",
            &[
                ("pool", &state.providers.health_pool.trim()),
                ("count", &count),
                ("all", &state.backing_remotes.len()),
            ],
        )
    } else {
        trf("Connected providers · {count}", &[("count", &count)])
    };
    let clicked = theme::card_section(
        ui,
        &title,
        None,
        |ui| {
            refresh = ui
                .add_enabled(!task.is_running(), egui::Button::new(tr("Refresh")))
                .clicked();
        },
        |ui| {
            if count == 0 {
                theme::hint(
                    ui,
                    if scoped {
                        tr("No connected provider belongs to this pool. Choose all crypt remotes as the scope to see every provider.")
                    } else {
                        tr("No providers yet. Connect one above.")
                    },
                );
                return None::<(card::CardAction, String)>;
            }
            let running = task.is_running()
                && task.task_name() == Some(crate::gui::app::AUTO_ENCRYPTION_TASK);
            let idle = !task.is_running() && state.providers.connection.is_none();
            // Nine times the usual list height: the cards are tall and the
            // page scrolls as a whole anyway.
            let height = 9.0 * theme::list_height(ui.ctx().content_rect().height());
            // Never shorter than one row of cards, however small the window:
            // the row height is measured on the previous frame.
            let row_id = egui::Id::new("base-provider-list-row-height");
            let row_height = ui.ctx().data(|d| d.get_temp::<f32>(row_id)).unwrap_or(0.0);
            let mut clicked = None;
            egui::ScrollArea::vertical()
                .id_salt("base-provider-list")
                .min_scrolled_height(row_height)
                .max_height(height.max(row_height))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let cards = model::cards(state, running);
                    let mut tallest = 0.0f32;
                    let per_row = grid::columns(ui.available_width(), cards.len());
                    for (row, chunk) in cards.chunks(per_row).enumerate() {
                        if row > 0 {
                            ui.add_space(grid::GAP);
                        }
                        let row_rect = ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = grid::GAP;
                            let gaps = grid::GAP * (per_row - 1) as f32;
                            let width = ((ui.available_width() - gaps) / per_row as f32).floor();
                            for card in chunk {
                                let size = egui::vec2(width, 0.0);
                                let layout = egui::Layout::top_down(egui::Align::Min);
                                ui.allocate_ui_with_layout(size, layout, |ui| {
                                    ui.set_width(width);
                                    if let Some(action) = card::show(ui, card, idle) {
                                        clicked = Some((action, card.name.to_string()));
                                    }
                                });
                            }
                        });
                        tallest = tallest.max(row_rect.response.rect.height());
                    }
                    ui.ctx().data_mut(|d| d.insert_temp(row_id, tallest));
                });
            clicked
        },
    );
    if refresh {
        state.providers.refresh_requested = true;
        state.providers.limits.invalidate();
    }
    match clicked {
        Some((card::CardAction::SetupEncryption, name)) => {
            state.providers.apply_defaults(&state.settings.encryption);
            state.providers.backing_provider = name;
            state.providers.setup_open = true;
        }
        Some((card::CardAction::EditLimits, name)) => {
            let kind = state
                .provider_details
                .kinds
                .get(&name)
                .cloned()
                .unwrap_or_default();
            state.providers.limits_editor =
                limits_dialog::LimitsEditor::open_for(&name, &kind, &state.providers.limits.store);
        }
        Some((card::CardAction::NameEncoding, name)) => {
            let crypts = state
                .provider_details
                .crypts
                .get(&name)
                .cloned()
                .unwrap_or_default();
            state.providers.name_encoding = name_encoding::Editor::open_for(&crypts);
        }
        Some((card::CardAction::KeepAlive, name)) => {
            state.providers.setup_notice = Some(match start_keepalive(state, task, &name) {
                Ok(()) => trf(
                    "Keeping {account} alive. See Jobs for the result.",
                    &[("account", &name)],
                ),
                Err(error) => error,
            });
            state.providers.limits.invalidate();
        }
        None => {}
    }
}

/// Argv of `rpool provider keepalive --remote <account>`.
fn keepalive_args(account: &str) -> Vec<OsString> {
    vec![
        OsString::from("provider"),
        OsString::from("keepalive"),
        OsString::from("--remote"),
        OsString::from(account),
    ]
}

/// Starts the keep-alive task for one account.
fn start_keepalive(state: &GuiState, task: &mut TaskRunner, account: &str) -> Result<(), String> {
    task.start_rpool(
        "Keep account alive",
        &state.settings.rclone,
        keepalive_args(account),
    )
}

/// "Health monitor" card: scope (crypt remotes or a pool), optional remote
/// selection, and "Check health".
fn health_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(
        ui,
        tr("Health monitor"),
        Some(tr("Checks that each encrypted remote answers at its root.")),
        |_| {},
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Scope"));
                egui::ComboBox::from_id_salt("provider-health-pool")
                    .selected_text(if state.providers.health_pool.is_empty() {
                        tr("Crypt remotes")
                    } else {
                        state.providers.health_pool.as_str()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut state.providers.health_pool,
                            String::new(),
                            tr("Crypt remotes"),
                        );
                        for name in &state.pool_names {
                            ui.selectable_value(
                                &mut state.providers.health_pool,
                                name.clone(),
                                trf("Pool {name}", &[("name", name)]),
                            );
                        }
                    });
            });
            if state.providers.health_pool.is_empty() && !state.crypt_remotes.is_empty() {
                theme::hint(
                    ui,
                    tr("Tick remotes to check only those; none ticked checks all."),
                );
                egui::ScrollArea::vertical()
                    .id_salt("health-remotes")
                    .max_height(140.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for remote in &state.crypt_remotes {
                                let mut on = state.providers.health_remotes.contains(remote);
                                if ui.checkbox(&mut on, remote).changed() {
                                    if on {
                                        state.providers.health_remotes.push(remote.clone());
                                    } else {
                                        state.providers.health_remotes.retain(|r| r != remote);
                                    }
                                }
                            }
                        });
                    });
            }
            if theme::primary_button(ui, !task.is_running(), tr("Check health")).clicked() {
                state.providers.error = start_health(state, task).err();
            }
        },
    );
}

/// Moving shards off one provider; shown with the other account changes.
pub(crate) fn drain_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(ui, tr("Drain / migrate a provider"), Some(tr("Copies and fully verifies shard objects on a new crypt remote, then replaces the manifest. Old shards are deleted only if you ask, and last.")), |_| {}, |ui| {
        local_file_field(ui, tr("Manifest (local file or remote path)"), &mut state.providers.manifest, Some("rpool manifest"), &["json"]);
        egui::Grid::new("drain-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            let width = (ui.available_width() - 80.0).clamp(160.0, 320.0);
            ui.label(tr("From"));
            ui.add(egui::TextEdit::singleline(&mut state.providers.from).hint_text("old-crypt:rpool").desired_width(width));
            ui.end_row();
            ui.label(tr("To"));
            ui.add(egui::TextEdit::singleline(&mut state.providers.to).hint_text("new-crypt:rpool").desired_width(width));
            ui.end_row();
        });
        output_file_field(ui, tr("Output manifest (optional for local input)"), &mut state.providers.output);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut state.providers.dry_run, tr("Dry run"));
            ui.checkbox(&mut state.providers.delete_source, tr("Delete old shards after verified migration"));
            ui.checkbox(&mut state.providers.allow_risky, tr("Allow unverified / weaker failure-domain safety"));
        });
        let label = if state.providers.dry_run { tr("Preview drain") } else { tr("Drain provider") };
        let clicked = if state.providers.delete_source && !state.providers.dry_run {
            theme::danger_button(ui, !task.is_running(), label).clicked()
        } else {
            theme::primary_button(ui, !task.is_running(), label).clicked()
        };
        if clicked {
            state.providers.error = start_drain(state, task).err();
        }
        if let Some(error) = &state.providers.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
}

/// Starts the provider health task.
fn start_health(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    task.start_rpool(
        "Provider health",
        &state.settings.rclone,
        health_args(state),
    )
}

/// Argv of `rpool provider health` for the chosen pool or ticked remotes
/// (none ticked = all), with this PC's worker count.
fn health_args(state: &GuiState) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("health"),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
    ];
    if !state.providers.health_pool.is_empty() {
        args.push(OsString::from("--pool"));
        args.push(OsString::from(state.providers.health_pool.trim()));
    } else {
        for remote in &state.providers.health_remotes {
            args.push(OsString::from("--remote"));
            args.push(OsString::from(remote));
        }
    }
    args
}

/// Validates the drain inputs and starts `rpool provider drain`.
fn start_drain(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let manifest = state.providers.manifest.trim();
    let from = state.providers.from.trim();
    let to = state.providers.to.trim();
    if manifest.is_empty() || from.is_empty() || to.is_empty() {
        return Err(tr("Manifest, From, and To are required.").to_string());
    }
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("drain"),
        OsString::from(manifest),
        OsString::from("--from"),
        OsString::from(from),
        OsString::from("--to"),
        OsString::from(to),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
        OsString::from("--retries"),
        OsString::from(state.settings.retries.to_string()),
    ];
    if !state.providers.output.trim().is_empty() {
        args.push(OsString::from("--output"));
        args.push(OsString::from(state.providers.output.trim()));
    }
    if state.providers.dry_run {
        args.push(OsString::from("--dry-run"));
    }
    if state.providers.delete_source {
        args.push(OsString::from("--delete-source"));
    }
    if state.providers.allow_risky {
        args.push(OsString::from("--allow-risky"));
    }
    task.start_rpool("Provider drain", &state.settings.rclone, args)
}

/// "New encrypted provider" dialog: backing provider, name and key options;
/// requires the backup acknowledgement before creating.
fn encryption_dialog(ctx: &egui::Context, state: &mut GuiState, task: &mut TaskRunner) {
    if !state.providers.setup_open {
        return;
    }
    let mut open = true;
    egui::Window::new(tr("New encrypted provider")).id(egui::Id::new("provider-encryption-setup"))
        .open(&mut open).resizable(true).default_width(530.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            ui.label(tr("Create a new crypt provider around a connected cloud. Existing keys and files are never changed."));
            ui.add_enabled_ui(!task.is_running() && state.providers.connection.is_none(), |ui| {
                ui.label(tr("Backing provider (non-crypt remote name)"));
                egui::ComboBox::from_id_salt("crypt-backing-choice")
                    .selected_text(if state.providers.backing_provider.is_empty() { tr("Choose provider") } else { &state.providers.backing_provider })
                    .show_ui(ui, |ui| {
                        for name in &state.backing_remotes {
                            ui.selectable_value(&mut state.providers.backing_provider, name.clone(), name.as_str());
                        }
                    });
                ui.add(egui::TextEdit::singleline(&mut state.providers.backing_provider).hint_text(tr("e.g. google_1 (no colon)")));
                ui.label(tr("New encrypted provider name"));
                ui.add(egui::TextEdit::singleline(&mut state.providers.crypt_name).hint_text(tr("e.g. google_1_crypt")));
                ui.small(tr("Uses the backing provider's remote default path exactly, without an extra folder. Existing crypt folders and files are not moved."));
                if !state.providers.backing_provider.is_empty() {
                    let root = state.remote_roots.get(&state.providers.backing_provider).map(String::as_str).unwrap_or("");
                    ui.monospace(trf("Backing location: {location}", &[("location", &format!("{}:{}", state.providers.backing_provider, root))]));
                }
                let strengths = [tr("256 bits"), tr("128 bits"), tr("512 bits"), tr("1024 bits")];
                egui::ComboBox::from_label(tr("Generated password entropy"))
                    .selected_text(strengths[state.providers.entropy_index.min(3)])
                    .show_ui(ui, |ui| { for (i, label) in strengths.iter().enumerate() { ui.selectable_value(&mut state.providers.entropy_index, i, *label); } });
                ui.small(tr("This controls random password strength, not the cipher. File contents always use rclone crypt encryption."));
                let modes = [tr("Standard — encrypted filenames (recommended)"), tr("Obfuscate — reversible names, not strong filename secrecy"), tr("Off — original filenames visible")];
                egui::ComboBox::from_label(tr("Filename protection"))
                    .selected_text(modes[state.providers.filename_index.min(2)])
                    .show_ui(ui, |ui| { for (i, label) in modes.iter().enumerate() { ui.selectable_value(&mut state.providers.filename_index, i, *label); } });
                ui.label(tr("Name encoding"));
                name_encoding::combo(ui, "crypt-name-encoding", &mut state.providers.encoding_index);
                ui.checkbox(&mut state.providers.disable_directory_encryption, tr("Leave directory names visible"));
                ui.small(tr("Filename Off also leaves directory names visible regardless of the directory option."));
                ui.separator();
                ui.label(tr("Generated keys are saved only in your local rclone configuration. No recovery vault is created automatically. Back up that configuration securely or export an encrypted RPool package before uploading data."));
                ui.checkbox(&mut state.providers.backup_acknowledged, tr("I understand that losing these keys loses access to my data"));
                ui.small(tr("Close other rclone config editors before creating. Encrypted config files require the official rclone config wizard instead."));
                if ui.add_enabled(state.providers.backup_acknowledged, egui::Button::new(tr("Generate keys and create encrypted provider"))).clicked() {
                    match start_encryption(state, task) {
                        Ok(()) => state.providers.setup_notice = Some(tr("Creating encrypted provider. See Jobs for the result; refresh providers after completion.").into()),
                        Err(error) => state.providers.setup_notice = Some(error),
                    }
                }
            });
            if let Some(notice) = &state.providers.setup_notice { ui.label(notice); }
        });
    });
    state.providers.setup_open = open;
}

/// Starts `rpool provider encrypt` with the dialog's choices.
fn start_encryption(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let form = &state.providers;
    if form.crypt_name.trim().is_empty() || form.backing_provider.trim().is_empty() {
        return Err(
            tr("Choose a backing provider and enter a new encrypted provider name.").into(),
        );
    }
    let bits = [256, 128, 512, 1024][form.entropy_index.min(3)];
    let mode = ["standard", "obfuscate", "off"][form.filename_index.min(2)];
    let args: Vec<OsString> = vec![
        "provider".into(),
        "encrypt".into(),
        format!("--name={}", form.crypt_name.trim()).into(),
        format!("--provider={}", form.backing_provider.trim()).into(),
        format!("--entropy-bits={bits}").into(),
        format!("--filename-encryption={mode}").into(),
        format!(
            "--filename-encoding={}",
            crate::config_sync::provision::FILENAME_ENCODINGS[form
                .encoding_index
                .min(crate::config_sync::provision::FILENAME_ENCODINGS.len() - 1)]
        )
        .into(),
        format!(
            "--directory-encryption={}",
            !form.disable_directory_encryption
        )
        .into(),
    ];
    task.start_rpool("Create encrypted provider", &state.settings.rclone, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manual_setup_inherits_defaults_without_touching_identity() {
        let mut form = ProviderForm {
            crypt_name: "existing-draft".into(),
            backup_acknowledged: true,
            ..Default::default()
        };
        let defaults = crate::config_sync::provision::EncryptionDefaults::default();
        form.apply_defaults(&defaults);
        assert_eq!([256, 128, 512, 1024][form.entropy_index], 1024);
        assert_eq!(form.crypt_name, "existing-draft");
        assert!(!form.backup_acknowledged);
        let changed = crate::config_sync::provision::EncryptionDefaults {
            entropy_bits: 128,
            filename_encryption: "off".into(),
            directory_encryption: false,
            filename_encoding: "base32768".into(),
        };
        form.apply_defaults(&changed);
        assert_eq!([256, 128, 512, 1024][form.entropy_index], 128);
        assert_eq!(form.filename_index, 2);
        assert_eq!(form.encoding_index, name_encoding::index_of("base32768"));
        assert!(form.disable_directory_encryption);
    }

    #[test]
    fn health_checks_a_pool_or_the_ticked_remotes() {
        use clap::Parser;
        let mut state = GuiState::new(Default::default(), Default::default(), Default::default());
        state.providers.health_remotes = vec!["a_crypt:".into(), "b_crypt:".into()];
        let parse = |state: &GuiState| {
            let argv = std::iter::once(OsString::from("rpool")).chain(health_args(state));
            crate::cli::Cli::try_parse_from(argv).is_ok()
        };
        let args = health_args(&state);
        assert_eq!(args.iter().filter(|a| *a == "--remote").count(), 2);
        assert!(parse(&state));
        state.providers.health_pool = "family".into();
        let args = health_args(&state);
        assert!(args.iter().any(|a| a == "--pool") && !args.iter().any(|a| a == "--remote"));
        assert!(parse(&state));
    }

    #[test]
    fn keepalive_button_runs_a_parseable_cli_command() {
        use clap::Parser;
        let argv = std::iter::once(OsString::from("rpool")).chain(keepalive_args("gdrive_1"));
        assert!(crate::cli::Cli::try_parse_from(argv).is_ok());
    }

    #[test]
    fn readiness_does_not_claim_success_on_unknown_or_missing_provider() {
        assert_eq!(
            encryption_status(false, false, false, false),
            "Encryption status unknown"
        );
        assert_eq!(
            encryption_status(true, false, false, true),
            "Encryption configured"
        );
        assert_eq!(
            encryption_status(true, true, false, true),
            "Setup incomplete — retry"
        );
        assert_eq!(
            encryption_status(true, true, true, true),
            "Setting up encryption…"
        );
    }
}
