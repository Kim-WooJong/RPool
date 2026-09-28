use crate::gui::state::GuiState;
use crate::gui::task::{JobStatus, TaskRunner};
use crate::gui::widgets::remote_selector;
use crate::models::{Placement, PoolDefinition};
use crate::pool::{load_pool_store, remove_pool};
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug)]
pub(crate) struct PoolForm {
    pub(crate) selected: String,
    pub(crate) name: String,
    pub(crate) remotes: Vec<String>,
    pub(crate) manual_remote: String,
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) notice: Option<String>,
}

impl Default for PoolForm {
    fn default() -> Self {
        Self {
            selected: String::new(),
            name: String::new(),
            remotes: Vec::new(),
            manual_remote: String::new(),
            shard_mib: crate::config::constants::DEFAULT_SHARD_MIB,
            workers: crate::config::constants::DEFAULT_WORKERS,
            retries: crate::config::constants::DEFAULT_RETRIES,
            placement: Placement::RoundRobin,
            data_shards: crate::config::constants::DEFAULT_DATA_SHARDS,
            parity_shards: crate::config::constants::DEFAULT_PARITY_SHARDS,
            notice: None,
        }
    }
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Storage pools");
    ui.label("Create reusable provider groups and upload policy defaults.");
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("Existing pool");
        egui::ComboBox::from_id_salt("pool-existing")
            .selected_text(if state.pools.selected.is_empty() {
                "Select pool"
            } else {
                state.pools.selected.as_str()
            })
            .show_ui(ui, |ui| {
                for name in &state.pool_names {
                    ui.selectable_value(&mut state.pools.selected, name.clone(), name.as_str());
                }
            });
        if ui.button("Load").clicked() {
            load_selected(state);
        }
        if ui.button("Reload list").clicked() {
            refresh_pool_names(state);
        }
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Pool name");
        ui.add(egui::TextEdit::singleline(&mut state.pools.name).desired_width(280.0));
    });

    ui.add_space(8.0);
    remote_selector(
        ui,
        &mut state.pools.remotes,
        &state.crypt_remotes,
        &state.settings.default_remote_path,
        &state.remote_roots,
        &mut state.pools.manual_remote,
    );

    ui.add_space(8.0);
    egui::Grid::new("pool-options")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Shard size (MiB)");
            ui.add(egui::DragValue::new(&mut state.pools.shard_mib).range(1..=1024 * 1024));
            ui.end_row();

            ui.label("Workers");
            ui.add(egui::DragValue::new(&mut state.pools.workers).range(1..=256));
            ui.end_row();

            ui.label("Retries");
            ui.add(egui::DragValue::new(&mut state.pools.retries).range(0..=100));
            ui.end_row();

            ui.label("Placement");
            egui::ComboBox::from_id_salt("pool-placement")
                .selected_text(state.pools.placement.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.pools.placement,
                        Placement::RoundRobin,
                        Placement::RoundRobin.label(),
                    );
                    ui.selectable_value(
                        &mut state.pools.placement,
                        Placement::FreeRatio,
                        Placement::FreeRatio.label(),
                    );
                });
            ui.end_row();

            ui.label("Data shards (K)");
            ui.add(egui::DragValue::new(&mut state.pools.data_shards).range(1..=255));
            ui.end_row();

            ui.label("Parity shards (M)");
            ui.add(egui::DragValue::new(&mut state.pools.parity_shards).range(0..=254));
            ui.end_row();
        });

    if let Some(notice) = &state.pools.notice {
        ui.label(notice);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Save pool"))
            .clicked()
        {
            save_current(state, task);
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Remove selected"))
            .clicked()
        {
            remove_selected(state);
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("New / clear"))
            .clicked()
        {
            state.pools = PoolForm::default();
        }
    });
}

fn load_selected(state: &mut GuiState) {
    let name = state.pools.selected.clone();
    if name.is_empty() {
        state.pools.notice = Some("Select a pool first.".to_string());
        return;
    }

    match state
        .pool_definitions
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))
    {
        Ok(pool) => {
            state.pools.name = name;
            state.pools.remotes = pool.remotes;
            state.pools.shard_mib = pool.shard_mib;
            state.pools.workers = pool.workers;
            state.pools.retries = pool.retries;
            state.pools.placement = pool.placement;
            state.pools.data_shards = pool.data_shards;
            state.pools.parity_shards = pool.parity_shards;
            state.pools.notice = Some("Pool loaded.".to_string());
        }
        Err(error) => state.pools.notice = Some(format!("Failed to load pool: {error:#}")),
    }
}

fn save_current(state: &mut GuiState, task: &mut TaskRunner) {
    let name = state.pools.name.trim().to_string();
    let definition = PoolDefinition {
        remotes: state.pools.remotes.clone(),
        shard_mib: state.pools.shard_mib,
        workers: state.pools.workers,
        retries: state.pools.retries,
        placement: state.pools.placement,
        data_shards: state.pools.data_shards,
        parity_shards: state.pools.parity_shards,
    };

    let args = pool_save_args(&name, &definition);
    match task.start_rpool("Save pool", &state.settings.rclone, args) {
        Ok(()) => {
            state.pools.notice = Some("Saving pool…".into());
        }
        Err(error) => state.pools.notice = Some(error),
    }
}
fn pool_save_args(name: &str, pool: &PoolDefinition) -> Vec<OsString> {
    let mut args = vec!["pool".into(), "set".into()];
    for remote in &pool.remotes {
        args.push(format!("--remote={remote}").into());
    }
    for (flag, value) in [
        ("--shard-mib", pool.shard_mib.to_string()),
        ("--workers", pool.workers.to_string()),
        ("--retries", pool.retries.to_string()),
        ("--data-shards", pool.data_shards.to_string()),
        ("--parity-shards", pool.parity_shards.to_string()),
    ] {
        args.extend([flag.into(), value.into()]);
    }
    args.extend([
        "--placement".into(),
        match pool.placement {
            Placement::RoundRobin => "round-robin",
            Placement::FreeRatio => "free-ratio",
        }
        .into(),
    ]);
    args.extend(["--".into(), name.into()]);
    args
}
pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    let name = task
        .last_task()
        .and_then(|t| t.invocation.as_ref())
        .filter(|i| i.task_name == "Save pool")
        .and_then(|i| i.args.last())
        .map(|s| s.to_string_lossy().into_owned());
    if let Some(name) = name {
        if status == JobStatus::Completed {
            if refresh_pool_names(state) {
                state.pools.selected = name.clone();
                state.pools.notice = Some(format!("Saved '{name}'."));
            } else {
                state.pools.notice = Some(format!(
                    "Saved '{name}', but {}",
                    state.pools.notice.as_deref().unwrap_or("reload failed")
                ));
            }
        } else {
            state.pools.notice =
                Some("Pool save failed or was cancelled; inspect the task log.".into());
        }
    }
}

fn remove_selected(state: &mut GuiState) {
    let name = if state.pools.selected.is_empty() {
        state.pools.name.trim().to_string()
    } else {
        state.pools.selected.clone()
    };
    if name.is_empty() {
        state.pools.notice = Some("Select or enter a pool name first.".to_string());
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
            state.pools = PoolForm::default();
            refresh_pool_names(state);
            state.pools.notice = Some(format!("Removed '{name}' from {}", path.display()));
        }
        Err(error) => state.pools.notice = Some(format!("Failed to remove pool: {error:#}")),
    }
}

pub(crate) fn refresh_pool_names(state: &mut GuiState) -> bool {
    match load_pool_store() {
        Ok(store) => {
            state.pool_names = store.pools.keys().cloned().collect();
            state.pool_definitions = store.pools;
            true
        }
        Err(error) => {
            state.pools.notice = Some(format!("Failed to reload pools: {error:#}"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn pool_save_arguments_roundtrip_spaces_unicode_and_policy() {
        let definition = PoolDefinition {
            remotes: vec!["-crypt:폴더 with spaces".into()],
            shard_mib: 17,
            workers: 3,
            retries: 2,
            placement: Placement::FreeRatio,
            data_shards: 4,
            parity_shards: 2,
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
                } => {
                    assert_eq!(name, "-pool name");
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
