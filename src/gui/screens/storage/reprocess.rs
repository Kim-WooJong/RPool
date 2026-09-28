//! Explicit, preview-first conversion of selected archives; originals are retained.
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::pool::{build_plan, ReprocessPlan};
use crate::presentation::format_bytes;
use eframe::egui;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct ReprocessForm {
    target: String,
    selected: BTreeSet<String>,
    manual: String,
    download_mib_s: f64,
    upload_mib_s: f64,
    preview: Option<ReprocessPlan>,
    preview_signature: String,
    pending: Option<Receiver<Result<ReprocessPlan, String>>>,
    pending_signature: String,
    notice: Option<String>,
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Reprocess existing data");
    ui.small("Save pool changes first. Select archives explicitly; new copies use the saved pool policy. Originals are never deleted.");
    if !state.inventory.loaded {
        state.inventory.refresh();
    }
    if state.reprocess.target.is_empty() && !state.pools.selected.is_empty() {
        state.reprocess.target = state.pools.selected.clone();
    }
    let size = ui.available_size();
    let gap = ui.spacing().item_spacing;
    let height = (size.y - 64.0).max(1.0);
    let width = ((size.x - gap.x) / 2.0).max(1.0);
    ui.horizontal(|ui| {
        pane(ui, "reprocess-inputs", egui::vec2(width, height), |ui| {
            ui.strong("Target policy and selected archives");
            egui::ComboBox::from_id_salt("reprocess-target")
                .selected_text(if state.reprocess.target.is_empty() { "Select saved pool" } else { &state.reprocess.target })
                .show_ui(ui, |ui| {
                    for name in &state.pool_names {
                        ui.selectable_value(&mut state.reprocess.target, name.clone(), name);
                    }
                });
            ui.label("Assumed aggregate throughput (MiB/s; 0 = unknown)");
            ui.horizontal(|ui| {
                ui.label("Download");
                ui.add(egui::DragValue::new(&mut state.reprocess.download_mib_s).range(0.0..=1_000_000.0));
            });
            ui.horizontal(|ui| {
                ui.label("Upload");
                ui.add(egui::DragValue::new(&mut state.reprocess.upload_mib_s).range(0.0..=1_000_000.0));
            });
            ui.small("These are estimates, not measured speeds. Provider throttling, retries and encoding can add time.");
            ui.separator();
            if ui.button("Refresh local library").clicked() { state.inventory.refresh(); }
            if let Some(error) = &state.inventory.error { ui.label(error); }
            ui.small("All indexed archives are listed; remote overlap does not prove pool ownership.");
            for row in &state.inventory.rows {
                let source = &row.entry.manifest_source;
                let mut selected = state.reprocess.selected.contains(source);
                if ui.checkbox(&mut selected, format!("{} · {}", row.entry.original_name, format_bytes(row.entry.original_size)))
                    .on_hover_text(source).changed() {
                    if selected { state.reprocess.selected.insert(source.clone()); }
                    else { state.reprocess.selected.remove(source); }
                }
            }
            ui.separator();
            if ui.button("Add manifest files…").clicked() {
                if let Some(paths) = rfd::FileDialog::new().add_filter("Manifest", &["json"]).pick_files() {
                    for path in paths { state.reprocess.selected.insert(path.to_string_lossy().into_owned()); }
                }
            }
            ui.add(egui::TextEdit::singleline(&mut state.reprocess.manual).hint_text("Local manifest or crypt:path/manifest.json").desired_width(260.0));
            if ui.button("Add manifest path").clicked() {
                let source = state.reprocess.manual.trim().to_string();
                if !source.is_empty() { state.reprocess.selected.insert(source); state.reprocess.manual.clear(); }
            }
            ui.label(format!("{} selected", state.reprocess.selected.len()));
            let sources: Vec<_> = state.reprocess.selected.iter().cloned().collect();
            for source in sources {
                ui.horizontal(|ui| {
                    if ui.small_button("Remove").clicked() { state.reprocess.selected.remove(&source); }
                    ui.label(source);
                });
            }
        });
        let signature = signature(state);
        poll(state, &signature);
        if state.reprocess.preview_signature != signature { state.reprocess.preview = None; }
        pane(ui, "reprocess-preview", egui::vec2(width, height), |ui| {
            ui.strong("Calculation / estimated time");
            if let Some(plan) = &state.reprocess.preview {
                ui.label(format!("Archives: {}", plan.entries.len()));
                ui.label(format!("Original file data: {}", format_bytes(plan.input_bytes)));
                ui.label(format!("Additional remote storage: {}", format_bytes(plan.new_storage_bytes)));
                ui.label(format!("Download / verification: {}", format_bytes(plan.download_bytes)));
                ui.label(format!("Upload: {}", format_bytes(plan.upload_bytes)));
                ui.label(match plan.estimated_seconds {
                    Some(seconds) => format!("Estimated transfer time: {:.1} minutes ({:.2} hours)", seconds / 60.0, seconds / 3600.0),
                    None => "Estimated time: unknown — enter both assumed transfer rates, then calculate.".into(),
                });
                ui.small(&plan.estimate_note);
                ui.separator();
                ui.label(format!("Saved policy: {} MiB shards, {}+{}, {} workers", plan.target.shard_mib, plan.target.data_shards, plan.target.parity_shards, plan.target.workers));
                for remote in &plan.target.remotes { ui.monospace(remote); }
                ui.small("Temporary disk needs at least two copies of the largest restored file, plus parity/transfer scratch. Remote space must hold both original and new archives.");
                ui.label(format!("Plan: {}", plan.plan_path.display()));
            } else {
                ui.label("Select archives and calculate before starting. No existing archive is automatically assigned to a pool.");
            }
            if state.reprocess.pending.is_some() { ui.spinner(); ui.label("Reading manifests and calculating…"); }
            if let Some(notice) = &state.reprocess.notice { ui.separator(); ui.label(notice); }
        });
    });
    if state.reprocess.pending.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    ui.horizontal(|ui| {
        let can_plan = !task.is_running()
            && state.reprocess.pending.is_none()
            && !state.reprocess.selected.is_empty()
            && state.pool_definitions.contains_key(&state.reprocess.target);
        if ui
            .add_enabled(can_plan, egui::Button::new("Calculate / preview"))
            .clicked()
        {
            start_preview(state);
        }
        let can_apply = !task.is_running()
            && state.reprocess.preview.is_some()
            && state.reprocess.pending.is_none();
        if ui
            .add_enabled(can_apply, egui::Button::new("Create reprocessed copies"))
            .clicked()
        {
            if let Some(plan) = &state.reprocess.preview {
                let args = execute_args(&plan.plan_path);
                match task.start_rpool("Reprocess pool data", &state.settings.rclone, args) {
                    Ok(()) => {
                        state.reprocess.notice = Some(format!(
                            "Started. Originals retained. Plan / results: {}",
                            plan.plan_path.display()
                        ));
                        state.reprocess.preview = None;
                    }
                    Err(error) => state.reprocess.notice = Some(error),
                }
            }
        }
    });
    ui.small("Saving a pool only changes future uploads. This action explicitly creates verified copies of the selected archives.");
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
    serde_json::to_string(&(
        &state.reprocess.target,
        state.pool_definitions.get(&state.reprocess.target),
        &state.reprocess.selected,
        state.reprocess.download_mib_s,
        state.reprocess.upload_mib_s,
        &state.settings.rclone,
    ))
    .unwrap_or_default()
}
fn start_preview(state: &mut GuiState) {
    let Some(target) = state.pool_definitions.get(&state.reprocess.target).cloned() else {
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
                    Some("Selection or settings changed; calculate a new preview.".into());
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
            state.reprocess.notice = Some("Calculation worker stopped; calculate again.".into());
        }
        _ => {}
    }
}
fn pane(ui: &mut egui::Ui, id: &str, size: egui::Vec2, show: impl FnOnce(&mut egui::Ui)) {
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
        .show(&mut child, show);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
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
        crate::gui::task::JobStatus::Completed => "Verified new copies added to the library; originals retained.",
        crate::gui::task::JobStatus::Cancelled => "Cancelled. Originals retained; completed copies and partial attempt receipts may remain. See task log.",
        _ => "Stopped with errors. Originals retained; completed copies and partial attempt receipts may remain. See task log.",
    };
    state.reprocess.notice = Some(detail.into());
}
