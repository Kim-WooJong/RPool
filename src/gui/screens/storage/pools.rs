//! Storage › Pools: load, edit, save (`rpool pool set` via the task runner)
//! and remove pool definitions, with the capacity estimate, account
//! identities and the speed test, metadata and retention cards below.
use super::pool_picker::PoolPicker;
use crate::gui::i18n::{tr, trf};
use crate::gui::settings::GuiSettings;
use crate::gui::state::{GuiState, StorageSection};
use crate::gui::task::{JobStatus, TaskRunner};
use crate::gui::theme;
use crate::models::{Placement, PoolDefinition};
use crate::pool::{load_pool_store, remove_pool};
use eframe::egui;
use std::ffi::OsString;

/// Pool editor state, held in `GuiState::pools`; mirrors a `PoolDefinition`.
#[derive(Debug)]
pub(crate) struct PoolForm {
    /// Pool chosen in the "Existing pool" combo box.
    pub(crate) selected: String,
    /// Name the pool is saved under.
    pub(crate) name: String,
    /// Destinations (crypt remotes) of the pool.
    pub(crate) remotes: Vec<String>,
    /// Text box of the "Advanced: custom destination" entry.
    pub(crate) manual_remote: String,
    /// Shard size in MiB.
    pub(crate) shard_mib: u64,
    /// Provider per-object limit in bytes; 0 means no limit.
    pub(crate) max_object_bytes: u64,
    /// Encrypt in RPool instead of through rclone crypt (put and reprocess).
    pub(crate) native_crypt: bool,
    /// Upload the drive's small files as packs (`--small-file-packing`).
    pub(crate) small_file_packing: bool,
    /// Shard transfers per pool operation (`--workers`).
    pub(crate) workers: usize,
    /// Retry count for rclone transfers (`--retries`).
    pub(crate) retries: u32,
    /// How shards are distributed over the destinations.
    pub(crate) placement: Placement,
    /// Data shards per group (K).
    pub(crate) data_shards: usize,
    /// Parity shards per group (M).
    pub(crate) parity_shards: usize,
    /// Result of the last load/save/remove, shown under the policy card.
    pub(crate) notice: Option<String>,
    /// The picker asked for a provider refresh; `gui::app` takes and handles it.
    pub(crate) refresh_requested: bool,
    /// Pool whose last save changed stored-data policy: offer a migration.
    pub(crate) migration_hint: Option<String>,
    /// Provider picker window for `remotes`.
    picker: PoolPicker,
    /// Background capacity estimate of the edited draft.
    capacity: crate::gui::widgets::pool_capacity::CapacityPreview,
    /// Editable account identity rows of the draft's backing accounts.
    identity_rows: Vec<crate::gui::widgets::account_identities::IdentityRow>,
    /// Backings the rows were built from; rebuilt when a new report arrives.
    identity_source: String,
}

impl PoolForm {
    /// A new-pool form prefilled from the GUI settings (new pools default to native crypt).
    pub(crate) fn from_settings(settings: &GuiSettings) -> Self {
        Self {
            selected: String::new(),
            name: String::new(),
            remotes: settings.remotes.clone(),
            manual_remote: String::new(),
            shard_mib: settings.shard_mib,
            max_object_bytes: 0,
            // New pools encrypt in RPool (rclone crypt format); loaded pools keep theirs.
            native_crypt: true,
            small_file_packing: false,
            workers: settings.workers,
            retries: settings.retries,
            placement: settings.placement,
            data_shards: settings.data_shards,
            parity_shards: settings.parity_shards,
            notice: None,
            refresh_requested: false,
            migration_hint: None,
            picker: PoolPicker::default(),
            capacity: Default::default(),
            identity_rows: Vec::new(),
            identity_source: String::new(),
        }
    }

    /// Fills the form from a saved pool definition.
    fn load_definition(&mut self, name: String, pool: PoolDefinition) {
        self.picker = PoolPicker::default();
        self.name = name;
        self.remotes = pool.remotes;
        self.shard_mib = pool.shard_size.mib_ceil();
        self.max_object_bytes = pool.max_object_bytes.unwrap_or(0);
        self.native_crypt = pool.native_crypt;
        self.small_file_packing = pool.small_file_packing;
        self.workers = pool.workers;
        self.retries = pool.retries;
        self.placement = pool.placement;
        self.data_shards = pool.data_shards;
        self.parity_shards = pool.parity_shards;
        self.notice = Some(tr("Pool loaded.").to_string());
    }

    /// `max_object_bytes` as an optional limit (0 = none).
    fn object_limit(&self) -> Option<u64> {
        (self.max_object_bytes > 0).then_some(self.max_object_bytes)
    }
}

/// Renders the Pools tab (`storage::show`) and the provider picker window.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.add_enabled_ui(!state.pools.picker.is_open(), |ui| {
        theme::page_body(ui, "pools", |ui| {
            theme::page_header(ui, tr("Storage pools"), Some(tr("Group encrypted providers and set the upload policy. Account identities decide how much space is really usable.")));

            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Existing pool"));
                egui::ComboBox::from_id_salt("pool-existing")
                    .selected_text(if state.pools.selected.is_empty() {
                        tr("Select pool")
                    } else {
                        state.pools.selected.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for name in &state.pool_names {
                            ui.selectable_value(&mut state.pools.selected, name.clone(), name.as_str());
                        }
                    });
                if ui.button(tr("Load")).clicked() {
                    load_selected(state);
                }
                if ui.button(tr("Reload list")).clicked() {
                    refresh_pool_names(state);
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(tr("Pool name"));
                ui.add(egui::TextEdit::singleline(&mut state.pools.name).desired_width(280.0));
            });

            ui.add_space(8.0);
            theme::two_up(
                ui,
                &mut (&mut *state, &mut *task),
                |ui, s| providers_card(ui, s.0),
                |ui, s| policy_card(ui, s.0, s.1),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!task.is_running(), egui::Button::new(tr("Save pool")))
                    .clicked()
                {
                    save_current(state, task);
                }
                if ui
                    .add_enabled(!task.is_running(), egui::Button::new(tr("Remove selected")))
                    .clicked()
                {
                    remove_selected(state);
                }
                if ui
                    .add_enabled(!task.is_running(), egui::Button::new(tr("New / clear")))
                    .clicked()
                {
                    state.pools = PoolForm::from_settings(&state.settings);
                }
            });
            migration_hint(ui, state);
            ui.add_space(theme::SECTION_GAP);
            super::speed_test::pool_card(ui, state, task);
            ui.add_space(theme::SECTION_GAP);
            super::metadata_card::pool_card(ui, state, task);
            ui.add_space(theme::SECTION_GAP);
            super::retention_card::pool_card(ui, state, task);
        });
    });
    let action = state
        .pools
        .picker
        .show(ui.ctx(), &mut state.pools.remotes, &state.crypt_remotes);
    state.pools.refresh_requested |= action.refresh;
    if action.setup {
        state.storage_section = StorageSection::Providers;
    }
}

/// After a save that changed remotes / K / M / shard size / native crypt.
fn migration_hint(ui: &mut egui::Ui, state: &mut GuiState) {
    let Some(name) = state.pools.migration_hint.clone() else {
        return;
    };
    ui.add_space(8.0);
    let (fill, fg) = theme::warning_colors(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(fill)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(fg, trf("'{name}' changed how data is stored. Existing archives still use the old accounts or coding until they are migrated.", &[("name", &name)]));
            ui.horizontal_wrapped(|ui| {
                if theme::primary_button(ui, true, tr("Plan migration now")).clicked() {
                    state.migration.select_pool(&name);
                    state.storage_section = StorageSection::Changes;
                    state.pools.migration_hint = None;
                }
                if ui.button(tr("Later")).clicked() {
                    state.pools.migration_hint = None;
                }
            });
        });
}

/// "Encrypted providers" card: picker button, selected destinations and a
/// custom destination entry.
fn providers_card(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(
        ui,
        tr("Encrypted providers"),
        Some(tr("Only selected destinations belong to this pool.")),
        |_| {},
        |ui| {
            if ui.button(tr("Choose encrypted providers…")).clicked() {
                state.pools.picker.open(&state.pools.remotes);
            }
            ui.small(trf("{n} selected", &[("n", &state.pools.remotes.len())]));
            for remote in &state.pools.remotes {
                ui.monospace(remote);
            }
            ui.collapsing(tr("Advanced: custom destination"), |ui| {
                ui.text_edit_singleline(&mut state.pools.manual_remote);
                if ui.button(tr("Add custom destination")).clicked() {
                    let target = state.pools.manual_remote.trim().to_string();
                    if !target.is_empty() && !state.pools.remotes.contains(&target) {
                        state.pools.remotes.push(target);
                        state.pools.manual_remote.clear();
                    }
                }
            });
        },
    );
}

/// "Pool policy" card: shard size, object limit, native crypt, retries,
/// placement and coding, the capacity estimate and account identities.
fn policy_card(ui: &mut egui::Ui, state: &mut GuiState, task: &TaskRunner) {
    theme::card_section(
        ui,
        tr("Pool policy"),
        Some(tr(
            "Saving affects future uploads. Existing data stays readable and unchanged.",
        )),
        |_| {},
        |ui| {
            if ui.button(tr("Reprocess existing data…")).clicked() {
                state.storage_section = StorageSection::Changes;
            }

            ui.add_space(8.0);
            egui::Grid::new("pool-options")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label(tr("Shard size (MiB)"));
                    ui.add(
                        egui::DragValue::new(&mut state.pools.shard_mib)
                            .range(1..=crate::config::constants::MAX_SHARD_MIB),
                    );
                    ui.end_row();

                    ui.label(tr("Provider object limit (bytes, 0 = none)"));
                    ui.add(egui::DragValue::new(&mut state.pools.max_object_bytes));
                    ui.end_row();

                    ui.label(tr("Native crypt"))
                        .on_hover_text(tr("RPool itself encrypts file contents and names in rclone crypt format for uploads, reprocessing and mounted drives, writing to each crypt remote's base. Readback goes through the rclone crypt remote, so rclone can always read the data. Crypt remotes with settings RPool does not support are refused."));
                    ui.checkbox(&mut state.pools.native_crypt, tr("Encrypt in RPool"));
                    ui.end_row();

                    ui.label(tr("Small-file packing"))
                        .on_hover_text(tr("The mounted drive uploads files of up to 1 MiB together as one archive per batch: far fewer requests and less space per small file. Every PC that uses this pool needs RPool 2.10 or later; older versions stop syncing the pool."));
                    ui.checkbox(&mut state.pools.small_file_packing, tr("Pack small files"));
                    ui.end_row();

                    ui.label(tr("Retries"));
                    ui.add(egui::DragValue::new(&mut state.pools.retries).range(0..=100));
                    ui.end_row();

                    ui.label(tr("Placement"));
                    egui::ComboBox::from_id_salt("pool-placement")
                        .selected_text(crate::gui::i18n::tr(state.pools.placement.label()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.pools.placement,
                                Placement::RoundRobin,
                                crate::gui::i18n::tr(Placement::RoundRobin.label()),
                            );
                            ui.selectable_value(
                                &mut state.pools.placement,
                                Placement::FreeRatio,
                                crate::gui::i18n::tr(Placement::FreeRatio.label()),
                            );
                            ui.selectable_value(
                                &mut state.pools.placement,
                                Placement::Proportional,
                                crate::gui::i18n::tr(Placement::Proportional.label()),
                            );
                            ui.selectable_value(
                                &mut state.pools.placement,
                                Placement::Resilient,
                                crate::gui::i18n::tr(Placement::Resilient.label()),
                            );
                            ui.selectable_value(
                                &mut state.pools.placement,
                                Placement::CapacityFirst,
                                crate::gui::i18n::tr(Placement::CapacityFirst.label()),
                            );
                        });
                    ui.end_row();

                    ui.label(tr("Data shards (K)"));
                    ui.add(egui::DragValue::new(&mut state.pools.data_shards).range(1..=255));
                    ui.end_row();

                    ui.label(tr("Parity shards (M)"));
                    ui.add(egui::DragValue::new(&mut state.pools.parity_shards).range(0..=254));
                    ui.end_row();
                });
            if let Some(note) = state.pools.placement.protection_note() {
                ui.small(crate::gui::i18n::tr(note));
            }

            let shard_size = crate::models::shard_size::ShardSize::from_mib(state.pools.shard_mib)
                .unwrap_or_default();
            let draft = PoolDefinition {
                remotes: state.pools.remotes.clone(),
                shard_size,
                workers: state.pools.workers,
                retries: state.pools.retries,
                placement: state.pools.placement,
                data_shards: state.pools.data_shards,
                parity_shards: state.pools.parity_shards,
                max_object_bytes: state.pools.object_limit(),
                native_crypt: state.pools.native_crypt,
                small_file_packing: state.pools.small_file_packing,
            };
            if let Err(error) = crate::models::shard_size::check_object_limit(
                shard_size.bytes(),
                draft.max_object_bytes,
            ) {
                ui.colored_label(ui.visuals().warn_fg_color, error.to_string());
            }
            state
                .pools
                .capacity
                .show(ui, &state.settings.rclone, &draft);
            identities(ui, state, &draft, !task.is_running());
            if let Some(notice) = &state.pools.notice {
                ui.label(notice);
            }
        },
    );
}

/// Account identities of this pool's backing accounts, right under the
/// capacity estimate that depends on them. Saving recalculates the estimate.
fn identities(ui: &mut egui::Ui, state: &mut GuiState, draft: &PoolDefinition, enabled: bool) {
    use crate::gui::widgets::account_identities::{editor, merged, pool_rows, EditorAction};
    use crate::storage::admin::domains::DomainStore;
    let backings = state
        .pools
        .capacity
        .report()
        .map(|r| r.backings.clone())
        .unwrap_or_default();
    let source = format!("{backings:?}");
    if source != state.pools.identity_source {
        state.pools.identity_rows = pool_rows(&DomainStore::load().unwrap_or_default(), &backings);
        state.pools.identity_source = source;
    }
    ui.separator();
    ui.strong(tr("Account identities"));
    ui.small(tr("Which accounts are independent (their free space adds up) and which can fail together. The estimate above uses these."));
    if editor(
        ui,
        "pool-identity-grid",
        &mut state.pools.identity_rows,
        enabled,
    ) == EditorAction::Save
    {
        let saved = DomainStore::load()
            .and_then(|store| merged(store, &state.pools.identity_rows))
            .and_then(|store| store.save());
        state.pools.notice = Some(match saved {
            Ok(()) => {
                state.pools.identity_source.clear();
                state.pools.capacity.start(&state.settings.rclone, draft);
                state.mount.invalidate_capacity();
                tr("Identities saved; recalculating capacity.").into()
            }
            Err(error) => format!("{error:#}"),
        });
    }
}

/// Loads the selected saved pool into the form; also used by the Drive page's "Edit in Pools".
pub(crate) fn load_selected(state: &mut GuiState) {
    let name = state.pools.selected.clone();
    if name.is_empty() {
        state.pools.notice = Some(tr("Select a pool first.").to_string());
        return;
    }

    match state
        .pool_definitions
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))
    {
        Ok(pool) => {
            state.pools.load_definition(name, pool);
        }
        Err(error) => {
            state.pools.notice = Some(trf(
                "Failed to load pool: {error}",
                &[("error", &format!("{error:#}"))],
            ))
        }
    }
}

/// Validates the shard size and starts `rpool pool set` for the form as a task.
fn save_current(state: &mut GuiState, task: &mut TaskRunner) {
    let name = state.pools.name.trim().to_string();
    let shard_size = match crate::models::shard_size::ShardSize::from_mib(state.pools.shard_mib) {
        Ok(size) => size,
        Err(error) => {
            state.pools.notice = Some(error.to_string());
            return;
        }
    };
    let definition = PoolDefinition {
        remotes: state.pools.remotes.clone(),
        shard_size,
        workers: state.pools.workers,
        retries: state.pools.retries,
        placement: state.pools.placement,
        data_shards: state.pools.data_shards,
        parity_shards: state.pools.parity_shards,
        max_object_bytes: state.pools.object_limit(),
        native_crypt: state.pools.native_crypt,
        small_file_packing: state.pools.small_file_packing,
    };

    let args = pool_save_args(&name, &definition);
    match task.start_rpool("Save pool", &state.settings.rclone, args) {
        Ok(()) => {
            state.pools.notice = Some(tr("Saving pool…").into());
        }
        Err(error) => state.pools.notice = Some(error),
    }
}
/// Argv of `rpool pool set` for `pool` saved as `name` (name last, after `--`).
fn pool_save_args(name: &str, pool: &PoolDefinition) -> Vec<OsString> {
    let mut args = vec!["pool".into(), "set".into()];
    for remote in &pool.remotes {
        args.push(format!("--remote={remote}").into());
    }
    for (flag, value) in [
        ("--shard-mib", pool.shard_size.to_string()),
        ("--workers", pool.workers.to_string()),
        ("--retries", pool.retries.to_string()),
        ("--data-shards", pool.data_shards.to_string()),
        ("--parity-shards", pool.parity_shards.to_string()),
    ] {
        args.extend([flag.into(), value.into()]);
    }
    if let Some(limit) = pool.max_object_bytes {
        args.extend(["--max-object-bytes".into(), limit.to_string().into()]);
    }
    if pool.native_crypt {
        args.push("--native-crypt".into());
    }
    if pool.small_file_packing {
        args.push("--small-file-packing".into());
    }
    args.extend(["--placement".into(), pool.placement.cli_value().into()]);
    args.extend(["--".into(), name.into()]);
    args
}
/// After a "Save pool" task: reloads the pool list and offers a migration when
/// the saved change affects stored data. Called from `gui::app`.
pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    let name = task
        .last_task()
        .and_then(|t| t.invocation.as_ref())
        .filter(|i| i.task_name == "Save pool")
        .and_then(|i| i.args.last())
        .map(|s| s.to_string_lossy().into_owned());
    if let Some(name) = name {
        if status == JobStatus::Completed {
            let before = state.pool_definitions.get(&name).cloned();
            if refresh_pool_names(state) {
                state.pools.selected = name.clone();
                state.pools.notice = Some(trf("Saved '{name}'.", &[("name", &name)]));
                let changed = matches!(
                    (&before, state.pool_definitions.get(&name)),
                    (Some(old), Some(new))
                        if super::migration::state::policy_change_affects_data(old, new)
                );
                state.pools.migration_hint = changed.then(|| name.clone());
            } else {
                state.pools.notice = Some(trf(
                    "Saved '{name}', but {reason}",
                    &[
                        ("name", &name),
                        (
                            "reason",
                            &state.pools.notice.as_deref().unwrap_or(tr("reload failed")),
                        ),
                    ],
                ));
            }
        } else {
            state.pools.notice =
                Some(tr("Pool save failed or was cancelled; inspect the task log.").into());
        }
    }
}

/// Removes the selected (or named) pool from the pool store and clears its
/// use in Upload and Manifest forms. Cloud data is not touched.
fn remove_selected(state: &mut GuiState) {
    let name = if state.pools.selected.is_empty() {
        state.pools.name.trim().to_string()
    } else {
        state.pools.selected.clone()
    };
    if name.is_empty() {
        state.pools.notice = Some(tr("Select or enter a pool name first.").to_string());
        return;
    }

    match remove_pool(&name) {
        Ok(path) => {
            if state.upload.pool_name == name {
                state.upload.pool_name.clear();
            }
            if state.manifest.pool_name == name {
                state.manifest.pool_name.clear();
            }
            state.pools = PoolForm::from_settings(&state.settings);
            refresh_pool_names(state);
            state.pools.notice = Some(trf(
                "Removed '{name}' from {path}",
                &[("name", &name), ("path", &path.display())],
            ));
        }
        Err(error) => {
            state.pools.notice = Some(trf(
                "Failed to remove pool: {error}",
                &[("error", &format!("{error:#}"))],
            ))
        }
    }
}

/// Reloads pool names and definitions from the pool store; false (with a
/// notice) when the store cannot be read.
pub(crate) fn refresh_pool_names(state: &mut GuiState) -> bool {
    match load_pool_store() {
        Ok(store) => {
            state.pool_names = store.pools.keys().cloned().collect();
            state.pool_definitions = store.pools;
            true
        }
        Err(error) => {
            state.pools.notice = Some(trf(
                "Failed to reload pools: {error}",
                &[("error", &format!("{error:#}"))],
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn new_pool_uses_current_settings_and_loaded_pool_keeps_its_policy() {
        let mut settings = GuiSettings {
            remotes: vec!["default-crypt:custom".into()],
            shard_mib: 17,
            workers: 3,
            retries: 9,
            placement: Placement::FreeRatio,
            data_shards: 7,
            parity_shards: 4,
            ..GuiSettings::default()
        };
        let mut form = PoolForm::from_settings(&settings);
        assert_eq!(form.remotes, settings.remotes);
        assert_eq!(
            (
                form.shard_mib,
                form.workers,
                form.retries,
                form.data_shards,
                form.parity_shards
            ),
            (17, 3, 9, 7, 4)
        );
        assert_eq!(form.placement, Placement::FreeRatio);
        let existing = PoolDefinition {
            remotes: vec!["existing-crypt:archive".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(29).unwrap(),
            workers: 2,
            retries: 1,
            placement: Placement::RoundRobin,
            data_shards: 3,
            parity_shards: 2,
            max_object_bytes: Some(250_000_000),
            native_crypt: true,
            small_file_packing: false,
        };
        form.load_definition("existing".into(), existing.clone());
        settings.shard_mib = 91;
        settings.workers = 6;
        assert_eq!(form.remotes, existing.remotes);
        assert_eq!(
            (
                form.shard_mib,
                form.workers,
                form.retries,
                form.data_shards,
                form.parity_shards
            ),
            (29, 2, 1, 3, 2)
        );
        assert_eq!(form.placement, Placement::RoundRobin);
        assert_eq!(form.object_limit(), Some(250_000_000));
        assert!(form.native_crypt);
        let reset = PoolForm::from_settings(&settings);
        assert_eq!((reset.shard_mib, reset.workers), (91, 6));
        assert_eq!(reset.object_limit(), None);
        assert!(reset.native_crypt, "new pools encrypt in RPool by default");
        assert!(reset.name.is_empty() && reset.selected.is_empty());
    }

    #[test]
    fn pool_save_arguments_roundtrip_spaces_unicode_and_policy() {
        let definition = PoolDefinition {
            remotes: vec!["-crypt:폴더 with spaces".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(17).unwrap(),
            workers: 3,
            retries: 2,
            placement: Placement::FreeRatio,
            data_shards: 4,
            parity_shards: 2,
            max_object_bytes: Some(250_000_000),
            native_crypt: true,
            small_file_packing: false,
        };
        let mut args = vec![
            OsString::from("rpool"),
            OsString::from("--rclone"),
            OsString::from(r"C:\Program Files\rclone.exe"),
        ];
        args.extend(pool_save_args("-pool name", &definition));
        let cli = crate::cli::Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.rclone, r"C:\Program Files\rclone.exe");
        match cli.command.unwrap() {
            crate::cli::Commands::Pool(args) => match args.command {
                crate::cli::PoolCommands::Set {
                    name,
                    remotes,
                    shard_mib,
                    workers,
                    retries,
                    placement,
                    data_shards,
                    parity_shards,
                    max_object_bytes,
                    native_crypt,
                    small_file_packing,
                } => {
                    assert_eq!(name, "-pool name");
                    assert!(!small_file_packing);
                    assert_eq!(max_object_bytes, Some(250_000_000));
                    assert!(native_crypt);
                    assert_eq!(remotes, definition.remotes);
                    assert_eq!(
                        (shard_mib, workers, retries, data_shards, parity_shards),
                        (17, 3, 2, 4, 2)
                    );
                    assert_eq!(placement, Placement::FreeRatio);
                }
                _ => panic!("wrong pool command"),
            },
            _ => panic!("wrong command"),
        }
    }
}
