use super::pool_picker::PoolPicker;
use crate::gui::settings::GuiSettings;
use crate::gui::state::{GuiState, StorageSection};
use crate::gui::task::{JobStatus, TaskRunner};
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
    /// Provider per-object limit in bytes; 0 means no limit.
    pub(crate) max_object_bytes: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) notice: Option<String>,
    pub(crate) refresh_requested: bool,
    picker: PoolPicker,
    capacity: crate::gui::widgets::pool_capacity::CapacityPreview,
}

impl PoolForm {
    pub(crate) fn invalidate_capacity(&mut self) {
        self.capacity.invalidate();
    }

    pub(crate) fn from_settings(settings: &GuiSettings) -> Self {
        Self {
            selected: String::new(),
            name: String::new(),
            remotes: settings.remotes.clone(),
            manual_remote: String::new(),
            shard_mib: settings.shard_mib,
            max_object_bytes: 0,
            workers: settings.workers,
            retries: settings.retries,
            placement: settings.placement,
            data_shards: settings.data_shards,
            parity_shards: settings.parity_shards,
            notice: None,
            refresh_requested: false,
            picker: PoolPicker::default(),
            capacity: Default::default(),
        }
    }

    fn load_definition(&mut self, name: String, pool: PoolDefinition) {
        self.picker = PoolPicker::default();
        self.name = name;
        self.remotes = pool.remotes;
        self.shard_mib = pool.shard_size.mib_ceil();
        self.max_object_bytes = pool.max_object_bytes.unwrap_or(0);
        self.workers = pool.workers;
        self.retries = pool.retries;
        self.placement = pool.placement;
        self.data_shards = pool.data_shards;
        self.parity_shards = pool.parity_shards;
        self.notice = Some("Pool loaded.".to_string());
    }

    fn object_limit(&self) -> Option<u64> {
        (self.max_object_bytes > 0).then_some(self.max_object_bytes)
    }
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.add_enabled_ui(!state.pools.picker.is_open(), |ui| {
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

        let gap = ui.spacing().item_spacing.x;
        let size = egui::vec2(
            ((ui.available_width() - gap) / 2.0).max(1.0),
            (ui.available_height() - 44.0).max(1.0),
        );
        ui.horizontal(|ui| {
            pane(ui, "pool-destinations-scroll", size, |ui| {
                ui.set_min_width(360.0);
                ui.add_space(8.0);
                ui.heading("Encrypted providers");
                ui.small("Only selected destinations belong to this pool.");
                if ui.button("Choose encrypted providers…").clicked() {
                    state.pools.picker.open(&state.pools.remotes);
                }
                ui.small(format!("{} selected", state.pools.remotes.len()));
                for remote in &state.pools.remotes {
                    ui.monospace(remote);
                }
                ui.collapsing("Advanced: custom destination", |ui| {
                    ui.text_edit_singleline(&mut state.pools.manual_remote);
                    if ui.button("Add custom destination").clicked() {
                        let target = state.pools.manual_remote.trim().to_string();
                        if !target.is_empty() && !state.pools.remotes.contains(&target) {
                            state.pools.remotes.push(target);
                            state.pools.manual_remote.clear();
                        }
                    }
                });
            });
            pane(ui, "pool-policy-scroll", size, |ui| {
                ui.heading("Pool policy");
                ui.label(
                    "Saving affects future uploads. Existing data stays readable and unchanged.",
                );
                if ui.button("Reprocess existing data…").clicked() {
                    state.storage_section = StorageSection::Reprocess;
                }

                ui.add_space(8.0);
                egui::Grid::new("pool-options")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Shard size (MiB)");
                        ui.add(
                            egui::DragValue::new(&mut state.pools.shard_mib)
                                .range(1..=crate::config::constants::MAX_SHARD_MIB),
                        );
                        ui.end_row();

                        ui.label("Provider object limit (bytes, 0 = none)");
                        ui.add(egui::DragValue::new(&mut state.pools.max_object_bytes));
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
                                ui.selectable_value(
                                    &mut state.pools.placement,
                                    Placement::Resilient,
                                    Placement::Resilient.label(),
                                );
                                ui.selectable_value(
                                    &mut state.pools.placement,
                                    Placement::CapacityFirst,
                                    Placement::CapacityFirst.label(),
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
                if let Some(note) = state.pools.placement.protection_note() {
                    ui.small(note);
                }

                let shard_size =
                    crate::models::shard_size::ShardSize::from_mib(state.pools.shard_mib)
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
                };
                if let Err(error) = crate::models::shard_size::check_object_limit(
                    shard_size.bytes(),
                    draft.max_object_bytes,
                ) {
                    ui.colored_label(ui.visuals().warn_fg_color, error.to_string());
                }
                if state
                    .pools
                    .capacity
                    .show(ui, &state.settings.rclone, &draft)
                {
                    state.storage_section = StorageSection::Mount;
                }
                if let Some(notice) = &state.pools.notice {
                    ui.label(notice);
                }
            });
        });

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
                state.pools = PoolForm::from_settings(&state.settings);
            }
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

// Allocate before rendering: expanding content cannot resize a neighboring pane.
pub(super) fn pane(
    ui: &mut egui::Ui,
    id: &str,
    size: egui::Vec2,
    content: impl FnOnce(&mut egui::Ui),
) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    egui::ScrollArea::both()
        .id_salt(id)
        .auto_shrink([false, false])
        .min_scrolled_width(0.0)
        .min_scrolled_height(0.0)
        .max_width(size.x)
        .max_height(size.y)
        .show(&mut child, content);
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
            state.pools.load_definition(name, pool);
        }
        Err(error) => state.pools.notice = Some(format!("Failed to load pool: {error:#}")),
    }
}

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
    args.extend(["--placement".into(), pool.placement.cli_value().into()]);
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
            state.pools = PoolForm::from_settings(&state.settings);
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
    fn storage_panes_keep_bounds_and_scroll_independently_at_minimum_window() {
        let ctx = egui::Context::default();
        let pane_size = egui::vec2(270.0, 160.0);
        let mut ids = [egui::Id::NULL; 2];
        let mut clips = [egui::Rect::NOTHING; 2];
        let mut row_bounds = egui::Rect::NOTHING;
        let mut draw = |events: Vec<egui::Event>, time: f64| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                // Representative content space after navigation, heading and console.
                row_bounds = ui
                    .horizontal(|ui| {
                        for (index, salt) in ["test-pool-destinations", "test-pool-policy"]
                            .iter()
                            .enumerate()
                        {
                            // egui 0.36 stores both builder salts as IdSalt before
                            // composing the child and ScrollArea persistent IDs.
                            let salt_id = egui::IdSalt::new(*salt);
                            ids[index] = ui.id().with(salt_id).with(salt_id);
                            pane(ui, salt, pane_size, |ui| {
                                clips[index] = ui.clip_rect();
                                ui.allocate_space(egui::vec2(900.0, 1200.0));
                            });
                        }
                    })
                    .response
                    .rect;
            })
            .drop_without_applying_deltas();
            (ids, clips, row_bounds)
        };
        draw(Vec::new(), 0.0);
        let (initial_ids, initial_clips, bounds) = draw(Vec::new(), 0.02);
        assert_ne!(initial_ids[0], initial_ids[1]);
        assert!(bounds.width() <= pane_size.x * 2.0 + 16.0);
        assert!(bounds.height() <= pane_size.y + 1.0);
        assert!(initial_clips[0].right() <= initial_clips[1].left());
        for clip in initial_clips {
            assert!(clip.width() <= pane_size.x + 1.0);
            assert!(clip.height() <= pane_size.y + 1.0);
        }
        let pointer = initial_clips[0].center();
        draw(vec![egui::Event::PointerMoved(pointer)], 0.04);
        let (scrolled_ids, _, _) = draw(
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -100.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            }],
            0.06,
        );
        assert_eq!(initial_ids, scrolled_ids);
        let first = egui::scroll_area::State::load(&ctx, initial_ids[0]).unwrap();
        let second = egui::scroll_area::State::load(&ctx, initial_ids[1]).unwrap();
        assert!(first.offset.y > 0.0, "wheel must scroll the hovered pane");
        assert_eq!(
            second.offset,
            egui::Vec2::ZERO,
            "neighbor must not inherit scrolling"
        );
    }

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
        let reset = PoolForm::from_settings(&settings);
        assert_eq!((reset.shard_mib, reset.workers), (91, 6));
        assert_eq!(reset.object_limit(), None);
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
                } => {
                    assert_eq!(name, "-pool name");
                    assert_eq!(max_object_bytes, Some(250_000_000));
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
