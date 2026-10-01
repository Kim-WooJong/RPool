//! Explicit, preview-first conversion of selected archives; originals are retained.
use super::pool_picker::PoolPicker;
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::models::{Placement, PoolDefinition};
use crate::pool::{
    build_plan, load_plan, upsert_pool, validate_pool, validate_pool_name, ReprocessPlan,
};
use crate::presentation::format_bytes;
use eframe::egui;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct ReprocessForm {
    target: String,
    loaded_target: String,
    draft: Option<PoolDefinition>,
    picker: PoolPicker,
    active_plan: Option<ReprocessPlan>,
    selected: BTreeSet<String>,
    manual: String,
    download_mib_s: f64,
    upload_mib_s: f64,
    preview: Option<ReprocessPlan>,
    preview_signature: String,
    pending: Option<Receiver<Result<ReprocessPlan, String>>>,
    pending_signature: String,
    pending_save: Option<Receiver<Result<(), String>>>,
    notice: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    poll_save(state);
    theme::hint(ui, tr("Edit a target draft, select archives, calculate, then execute. Draft changes do not save the pool policy. Originals and old provider connections are retained."));
    if !state.inventory.loaded {
        state.inventory.refresh();
    }
    if state.reprocess.target.is_empty() && !state.pools.selected.is_empty() {
        state.reprocess.target = state.pools.selected.clone();
    }
    theme::split_cards(ui, |ui, side| {
        if side == 0 {
            ui.strong(tr("Target policy and selected archives"));
            egui::ComboBox::from_id_salt("reprocess-target")
                .selected_text(if state.reprocess.target.is_empty() {
                    tr("Select saved pool")
                } else {
                    &state.reprocess.target
                })
                .show_ui(ui, |ui| {
                    for name in &state.pool_names {
                        ui.selectable_value(&mut state.reprocess.target, name.clone(), name);
                    }
                });
            if state.reprocess.loaded_target != state.reprocess.target {
                state.reprocess.picker = PoolPicker::default();
                state.reprocess.draft =
                    state.pool_definitions.get(&state.reprocess.target).cloned();
                state.reprocess.loaded_target = state.reprocess.target.clone();
            }
            if ui.button(tr("Reload saved policy into draft")).clicked() {
                state.reprocess.picker = PoolPicker::default();
                state.reprocess.draft =
                    state.pool_definitions.get(&state.reprocess.target).cloned();
            }
            if let Some(draft) = &mut state.reprocess.draft {
                ui.strong(tr("Target draft (not saved to pool)"));
                if ui.button(tr("Add / remove encrypted providers…")).clicked() {
                    state.reprocess.picker.open(&draft.remotes);
                }
                for remote in &draft.remotes {
                    ui.monospace(remote);
                }
                egui::Grid::new("reprocess-draft-policy")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(tr("Shard size (MiB)"));
                        let mut shard_mib = draft.shard_size.mib_ceil();
                        if ui
                            .add(
                                egui::DragValue::new(&mut shard_mib)
                                    .range(1..=crate::config::constants::MAX_SHARD_MIB),
                            )
                            .changed()
                        {
                            if let Ok(size) =
                                crate::models::shard_size::ShardSize::from_mib(shard_mib)
                            {
                                draft.shard_size = size;
                            }
                        }
                        ui.end_row();
                        ui.label(tr("Data shards (K)"));
                        ui.add(egui::DragValue::new(&mut draft.data_shards).range(1..=255));
                        ui.end_row();
                        ui.label(tr("Parity shards (M)"));
                        ui.add(egui::DragValue::new(&mut draft.parity_shards).range(0..=254));
                        ui.end_row();
                        ui.label(tr("Workers"));
                        ui.add(egui::DragValue::new(&mut draft.workers).range(1..=256));
                        ui.end_row();
                        ui.label(tr("Retries"));
                        ui.add(egui::DragValue::new(&mut draft.retries).range(0..=100));
                        ui.end_row();
                        ui.label(tr("Placement"));
                        egui::ComboBox::from_id_salt("reprocess-placement")
                            .selected_text(crate::gui::i18n::tr(draft.placement.label()))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut draft.placement,
                                    Placement::RoundRobin,
                                    crate::gui::i18n::tr(Placement::RoundRobin.label()),
                                );
                                ui.selectable_value(
                                    &mut draft.placement,
                                    Placement::FreeRatio,
                                    crate::gui::i18n::tr(Placement::FreeRatio.label()),
                                );
                                ui.selectable_value(
                                    &mut draft.placement,
                                    Placement::Resilient,
                                    crate::gui::i18n::tr(Placement::Resilient.label()),
                                );
                                ui.selectable_value(
                                    &mut draft.placement,
                                    Placement::CapacityFirst,
                                    crate::gui::i18n::tr(Placement::CapacityFirst.label()),
                                );
                            });
                        ui.end_row();
                    });
                if let Some(note) = draft.placement.protection_note() {
                    ui.small(crate::gui::i18n::tr(note));
                }
            }
            let can_save = !task.is_running()
                && state.reprocess.pending.is_none()
                && state.reprocess.pending_save.is_none()
                && !state.reprocess.picker.is_open()
                && validate_pool_name(&state.reprocess.target).is_ok()
                && state
                    .reprocess
                    .draft
                    .as_ref()
                    .is_some_and(|draft| validate_pool(draft).is_ok());
            if ui
                .add_enabled(
                    can_save,
                    egui::Button::new(tr("Save draft as pool defaults")),
                )
                .clicked()
            {
                start_save(state);
            }
            ui.small(tr("Optional: save this named pool's defaults for future uploads only. Existing archives are unchanged; conversion is a separate action."));
            if state.reprocess.pending_save.is_some() {
                ui.spinner();
                ui.label(tr("Saving pool defaults…"));
            }
            ui.label(tr("Assumed aggregate throughput (MiB/s; 0 = unknown)"));
            ui.horizontal(|ui| {
                ui.label(tr("Download"));
                ui.add(
                    egui::DragValue::new(&mut state.reprocess.download_mib_s)
                        .range(0.0..=1_000_000.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label(tr("Upload"));
                ui.add(
                    egui::DragValue::new(&mut state.reprocess.upload_mib_s)
                        .range(0.0..=1_000_000.0),
                );
            });
            ui.small(tr("These are estimates, not measured speeds. Provider throttling, retries and encoding can add time."));
            ui.separator();
            if ui.button(tr("Refresh local library")).clicked() {
                state.inventory.refresh();
            }
            if let Some(error) = &state.inventory.error {
                ui.label(error);
            }
            ui.small(tr(
                "All indexed archives are listed; remote overlap does not prove pool ownership.",
            ));
            for row in &state.inventory.rows {
                let source = &row.entry.manifest_source;
                let mut selected = state.reprocess.selected.contains(source);
                if ui
                    .checkbox(
                        &mut selected,
                        format!(
                            "{} · {}",
                            row.entry.original_name,
                            format_bytes(row.entry.original_size)
                        ),
                    )
                    .on_hover_text(source)
                    .changed()
                {
                    if selected {
                        state.reprocess.selected.insert(source.clone());
                    } else {
                        state.reprocess.selected.remove(source);
                    }
                }
            }
            ui.separator();
            if ui.button(tr("Add manifest files…")).clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter(tr("Manifest"), &["json"])
                    .pick_files()
                {
                    for path in paths {
                        state
                            .reprocess
                            .selected
                            .insert(path.to_string_lossy().into_owned());
                    }
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut state.reprocess.manual)
                    .hint_text(tr("Local manifest or crypt:path/manifest.json"))
                    .desired_width(260.0),
            );
            if ui.button(tr("Add manifest path")).clicked() {
                let source = state.reprocess.manual.trim().to_string();
                if !source.is_empty() {
                    state.reprocess.selected.insert(source);
                    state.reprocess.manual.clear();
                }
            }
            ui.label(trf(
                "{n} selected",
                &[("n", &state.reprocess.selected.len())],
            ));
            let sources: Vec<_> = state.reprocess.selected.iter().cloned().collect();
            for source in sources {
                ui.horizontal(|ui| {
                    if ui.small_button(tr("Remove")).clicked() {
                        state.reprocess.selected.remove(&source);
                    }
                    ui.label(source);
                });
            }
        } else {
            let signature = signature(state);
            poll(state, &signature);
            if state.reprocess.preview_signature != signature {
                state.reprocess.preview = None;
            }
            ui.strong(tr("Calculation / estimated time"));
            if let Some(plan) = &state.reprocess.preview {
                ui.label(trf("Archives: {n}", &[("n", &plan.entries.len())]));
                ui.label(trf(
                    "Original file data: {size}",
                    &[("size", &format_bytes(plan.input_bytes))],
                ));
                ui.label(trf(
                    "Additional remote storage: {size}",
                    &[("size", &format_bytes(plan.new_storage_bytes))],
                ));
                ui.label(trf(
                    "Download / verification: {size}",
                    &[("size", &format_bytes(plan.download_bytes))],
                ));
                ui.label(trf(
                    "Upload: {size}",
                    &[("size", &format_bytes(plan.upload_bytes))],
                ));
                ui.label(match plan.estimated_seconds {
                    Some(seconds) => trf("Estimated time until selected verified copies are ready: {minutes} minutes ({hours} hours)", &[("minutes", &format!("{:.1}", seconds / 60.0)), ("hours", &format!("{:.2}", seconds / 3600.0))]),
                    None => tr("Estimated time: unknown — enter both assumed transfer rates, then calculate.").into(),
                });
                ui.small(&plan.estimate_note);
                for change in &plan.change_summary {
                    ui.label(change);
                }
                ui.separator();
                ui.label(trf(
                    "Frozen target: {shard} MiB shards, {k}+{m}, {workers} workers, {retries} retries, {placement}",
                    &[
                        ("shard", &plan.target.shard_size),
                        ("k", &plan.target.data_shards),
                        ("m", &plan.target.parity_shards),
                        ("workers", &plan.target.workers),
                        ("retries", &plan.target.retries),
                        ("placement", &plan.target.placement.label()),
                    ],
                ));
                for remote in &plan.target.remotes {
                    ui.monospace(remote);
                }
                ui.small(tr("Temporary disk needs at least two copies of the largest restored file, plus parity/transfer scratch. Remote space must hold both original and new archives."));
                ui.label(trf("Plan: {path}", &[("path", &plan.plan_path.display())]));
            } else {
                ui.label(tr("Select archives and calculate before starting. No existing archive is automatically assigned to a pool."));
            }
            if let Some(plan) = &state.reprocess.active_plan {
                ui.separator();
                ui.strong(tr("Saved operation / resume"));
                ui.label(trf("Plan: {path}", &[("path", &plan.plan_path.display())]));
                ui.label(trf(
                    "Exact saved target: {shard} MiB, {k}+{m}, {workers} workers, {retries} retries, {placement}",
                    &[
                        ("shard", &plan.target.shard_size),
                        ("k", &plan.target.data_shards),
                        ("m", &plan.target.parity_shards),
                        ("workers", &plan.target.workers),
                        ("retries", &plan.target.retries),
                        ("placement", &plan.target.placement.label()),
                    ],
                ));
                for remote in &plan.target.remotes {
                    ui.monospace(remote);
                }
                ui.label(trf(
                    "{n} explicitly selected archives",
                    &[("n", &plan.entries.len())],
                ));
                for entry in &plan.entries {
                    ui.label(&entry.source);
                }
                ui.small(tr("Resume uses this saved plan, not the editable draft. Verified completed copies are rechecked and reused. The preview estimate is for the full operation, not remaining time."));
            }
            if state.reprocess.pending.is_some() {
                ui.spinner();
                ui.label(tr("Reading manifests and calculating…"));
            }
            if let Some(notice) = &state.reprocess.notice {
                ui.separator();
                ui.label(notice);
            }
        }
    });
    if state.reprocess.pending.is_some() || state.reprocess.pending_save.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    ui.horizontal_wrapped(|ui| {
        let can_plan = !task.is_running()
            && state.reprocess.pending.is_none()
            && state.reprocess.pending_save.is_none()
            && !state.reprocess.selected.is_empty()
            && state.reprocess.draft.is_some()
            && !state.reprocess.picker.is_open();
        if ui
            .add_enabled(can_plan, egui::Button::new(tr("Calculate / preview")))
            .clicked()
        {
            start_preview(state);
        }
        let can_apply = !task.is_running()
            && state.reprocess.preview.is_some()
            && state.reprocess.pending.is_none()
            && state.reprocess.pending_save.is_none()
            && !state.reprocess.picker.is_open();
        if ui
            .add_enabled(
                can_apply,
                egui::Button::new(tr("Create reprocessed copies")),
            )
            .clicked()
        {
            if let Some(plan) = &state.reprocess.preview {
                let args = execute_args(&plan.plan_path);
                match task.start_rpool("Reprocess pool data", &state.settings.rclone, args) {
                    Ok(()) => {
                        state.reprocess.notice = Some(trf(
                            "Started. Originals retained. Plan / results: {path}",
                            &[("path", &plan.plan_path.display())],
                        ));
                        state.reprocess.active_plan = Some(plan.clone());
                        state.reprocess.preview = None;
                    }
                    Err(error) => state.reprocess.notice = Some(error),
                }
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!task.is_running() && state.reprocess.pending.is_none()
            && state.reprocess.pending_save.is_none(), egui::Button::new(tr("Open saved plan…"))).clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter(tr("Reprocess plan"), &["json"]).pick_file() {
                load_active_plan(state, path);
            }
        }
        if ui.add_enabled(!task.is_running() && state.reprocess.active_plan.is_some() && state.reprocess.pending.is_none()
            && state.reprocess.pending_save.is_none(), egui::Button::new(tr("Resume saved operation"))).clicked() {
            let path = state.reprocess.active_plan.as_ref().unwrap().plan_path.clone();
            match load_plan(&path) {
                Ok(plan) => {
                    state.reprocess.active_plan = Some(plan);
                    match task.start_rpool("Reprocess pool data", &state.settings.rclone, execute_args(&path)) {
                        Ok(()) => state.reprocess.notice = Some(tr("Resuming saved operation; completed copies will be verified before reuse. Originals retained.").into()),
                        Err(error) => state.reprocess.notice = Some(error),
                    }
                }
                Err(error) => state.reprocess.notice = Some(trf("Cannot resume: {error}", &[("error", &format!("{error:#}"))])),
            }
        }
        if ui.add_enabled(!task.is_running() && !state.mount.is_running()
            && state.reprocess.active_plan.is_some(), egui::Button::new(tr("Use this plan below"))).clicked() {
            let path = state.reprocess.active_plan.as_ref().unwrap().plan_path.clone();
            state.mount.use_reprocess_plan(&path);
            state.reprocess.notice = Some(tr("Plan selected for “Apply changed pool” and “Recover after losing an account” below.").into());
        }
    });
    if let Some(draft) = &mut state.reprocess.draft {
        let action =
            state
                .reprocess
                .picker
                .show(ui.ctx(), &mut draft.remotes, &state.crypt_remotes);
        state.pools.refresh_requested |= action.refresh;
        if action.setup {
            state.storage_section = crate::gui::state::StorageSection::Providers;
        }
    }
    ui.small(tr("Saving a pool only changes future uploads. Saving draft defaults is optional and separate from conversion. This operation covers only selected archives, not necessarily the entire pool. No old provider data is deleted or disconnected."));
}

fn start_save(state: &mut GuiState) {
    let Some(draft) = state.reprocess.draft.clone() else {
        return;
    };
    let name = state.reprocess.target.clone();
    let rclone = state.settings.rclone.clone();
    let (tx, rx) = mpsc::channel();
    state.reprocess.pending_save = Some(rx);
    state.reprocess.notice = None;
    std::thread::spawn(move || {
        let result = upsert_pool(&rclone, &name, draft)
            .map(|_| ())
            .map_err(|error| {
                trf(
                    "Failed to save pool defaults: {error}",
                    &[("error", &format!("{error:#}"))],
                )
            });
        let _ = tx.send(result);
    });
}

fn poll_save(state: &mut GuiState) {
    match state
        .reprocess
        .pending_save
        .as_ref()
        .map(|rx| rx.try_recv())
    {
        Some(Ok(result)) => {
            state.reprocess.pending_save = None;
            state.reprocess.notice = Some(match result {
                Ok(()) => {
                    if super::pools::refresh_pool_names(state) {
                        tr("Pool defaults saved for future uploads. Existing archives were not changed; conversion remains a separate action.").into()
                    } else {
                        trf(
                            "Pool defaults saved, but {reason}",
                            &[(
                                "reason",
                                &state
                                    .pools
                                    .notice
                                    .as_deref()
                                    .unwrap_or(tr("pool list reload failed")),
                            )],
                        )
                    }
                }
                Err(error) => error,
            });
        }
        Some(Err(mpsc::TryRecvError::Disconnected)) => {
            state.reprocess.pending_save = None;
            state.reprocess.notice = Some(
                tr("Save worker stopped; reload the pool list to check the saved state.").into(),
            );
        }
        _ => {}
    }
}

fn load_active_plan(state: &mut GuiState, path: PathBuf) {
    match load_plan(&path) {
        Ok(plan) => {
            state.reprocess.active_plan = Some(plan);
            state.reprocess.notice = Some(
                tr("Saved plan loaded. Review its exact target and selected archives before resuming.")
                    .into(),
            );
        }
        Err(error) => {
            state.reprocess.notice = Some(trf(
                "Cannot load saved plan: {error}",
                &[("error", &format!("{error:#}"))],
            ))
        }
    }
}

fn execute_args(path: &std::path::Path) -> Vec<OsString> {
    vec![
        "pool".into(),
        "reprocess".into(),
        "--plan".into(),
        path.as_os_str().to_owned(),
    ]
}
fn signature(state: &GuiState) -> String {
    input_signature(&state.reprocess, &state.settings.rclone)
}
fn input_signature(form: &ReprocessForm, rclone: &str) -> String {
    serde_json::to_string(&(
        &form.target,
        &form.draft,
        &form.selected,
        form.download_mib_s,
        form.upload_mib_s,
        rclone,
    ))
    .unwrap_or_default()
}
fn start_preview(state: &mut GuiState) {
    let Some(target) = state.reprocess.draft.clone() else {
        return;
    };
    let sources = state.reprocess.selected.iter().cloned().collect();
    let rclone = state.settings.rclone.clone();
    let download = (state.reprocess.download_mib_s > 0.0).then_some(state.reprocess.download_mib_s);
    let upload = (state.reprocess.upload_mib_s > 0.0).then_some(state.reprocess.upload_mib_s);
    let (tx, rx) = mpsc::channel();
    state.reprocess.preview = None;
    state.reprocess.notice = None;
    state.reprocess.pending_signature = signature(state);
    state.reprocess.pending = Some(rx);
    std::thread::spawn(move || {
        let _ = tx.send(
            build_plan(&rclone, sources, target, download, upload).map_err(|e| format!("{e:#}")),
        );
    });
}
fn poll(state: &mut GuiState, signature: &str) {
    let received = state.reprocess.pending.as_ref().map(|rx| rx.try_recv());
    match received {
        Some(Ok(result)) => {
            state.reprocess.pending = None;
            if state.reprocess.pending_signature != signature {
                state.reprocess.notice =
                    Some(tr("Selection or settings changed; calculate a new preview.").into());
                return;
            }
            match result {
                Ok(plan) => {
                    state.reprocess.preview = Some(plan);
                    state.reprocess.preview_signature = signature.to_string();
                }
                Err(error) => state.reprocess.notice = Some(error),
            }
        }
        Some(Err(mpsc::TryRecvError::Disconnected)) => {
            state.reprocess.pending = None;
            state.reprocess.notice =
                Some(tr("Calculation worker stopped; calculate again.").into());
        }
        _ => {}
    }
}

pub(crate) fn handle_task_completion(
    state: &mut GuiState,
    task: &TaskRunner,
    status: crate::gui::task::JobStatus,
) {
    if task
        .last_task()
        .is_none_or(|last| last.name != "Reprocess pool data")
    {
        return;
    }
    state.inventory.refresh();
    let detail = match status {
        crate::gui::task::JobStatus::Completed => tr("Verified new copies added to the library; originals retained."),
        crate::gui::task::JobStatus::Cancelled => tr("Cancelled. Originals retained. Use Resume saved operation to continue this exact plan; see task log for completed and partial copies."),
        _ => tr("Stopped with errors. Originals retained. Fix the reported problem, then Resume saved operation; see task log for details."),
    };
    state.reprocess.notice = Some(detail.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn preview_invalidates_for_every_policy_source_rate_and_executor_change() {
        let mut form = ReprocessForm {
            target: "saved".into(),
            ..Default::default()
        };
        form.selected.insert("original.json".into());
        form.draft = Some(PoolDefinition {
            max_object_bytes: None,
            native_crypt: false,
            remotes: vec!["crypt:custom saved path".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(32).unwrap(),
            workers: 2,
            retries: 3,
            placement: Placement::RoundRobin,
            data_shards: 4,
            parity_shards: 2,
        });
        let original = form.draft.clone().unwrap();
        let baseline = input_signature(&form, "rclone");
        for mutate in [
            (|p: &mut PoolDefinition| {
                p.shard_size = crate::models::shard_size::ShardSize::from_mib(
                    p.shard_size.exact_mib().unwrap() + 1,
                )
                .unwrap()
            }) as fn(&mut PoolDefinition),
            |p| p.workers += 1,
            |p| p.retries += 1,
            |p| p.data_shards += 1,
            |p| p.parity_shards += 1,
            |p| p.placement = Placement::FreeRatio,
            |p| p.remotes.push("new:root".into()),
            |p| {
                p.remotes.remove(0);
            },
        ] {
            form.draft = Some(original.clone());
            mutate(form.draft.as_mut().unwrap());
            assert_ne!(input_signature(&form, "rclone"), baseline);
        }
        form.draft = Some(original);
        form.selected.insert("another.json".into());
        assert_ne!(input_signature(&form, "rclone"), baseline);
        form.selected.remove("another.json");
        form.download_mib_s = 1.0;
        assert_ne!(input_signature(&form, "rclone"), baseline);
        form.download_mib_s = 0.0;
        form.upload_mib_s = 1.0;
        assert_ne!(input_signature(&form, "rclone"), baseline);
        form.upload_mib_s = 0.0;
        assert_ne!(input_signature(&form, "other-rclone"), baseline);
        assert_eq!(input_signature(&form, "rclone"), baseline);
    }

    #[test]
    fn execute_plan_argument_preserves_windows_spaces_and_unicode() {
        let path = std::path::Path::new(r"C:\Users\Example Name\계획.json");
        let mut args = vec![OsString::from("rpool")];
        args.extend(execute_args(path));
        let cli = crate::cli::Cli::try_parse_from(args).unwrap();
        match cli.command.unwrap() {
            crate::cli::Commands::Pool(pool) => match pool.command {
                crate::cli::PoolCommands::Reprocess { plan } => assert_eq!(plan, path),
                _ => panic!("wrong pool command"),
            },
            _ => panic!("wrong command"),
        }
    }
}
