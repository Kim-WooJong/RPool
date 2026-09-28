use crate::gui::state::GuiState;
use crate::gui::task::{JobStatus, TaskRunner};
use eframe::egui;
use std::ffi::OsString;
use std::path::Path;

pub(crate) struct MountForm {
    pool: String,
    workspace: String,
    mountpoint: String,
    shared_root: String,
    worker_name: String,
    manifests: Vec<String>,
    manifest_input: String,
    interval_seconds: u64,
    runner: TaskRunner,
    control: Option<tempfile::TempDir>,
    stopping: bool,
    notice: Option<String>,
    capacity: Option<crate::mount::capacity::CapacityStatus>,
    capacity_read: std::time::Instant,
    identity_editor: String,
    virtual_drive: bool,
    bounded_shared: bool,
    pool_sync: bool,
    pool_retention: bool,
    pool_history_limit: u32,
    pool_history_override: bool,
    pool_status: Option<crate::mount::pool_sync::Status>,
    shared_coordinator: bool,
    shared_keep_previous: usize,
    cache_gib: u64,
}

impl Default for MountForm {
    fn default() -> Self {
        Self {
            pool: String::new(),
            workspace: String::new(),
            mountpoint: if cfg!(windows) {
                "R:".into()
            } else {
                String::new()
            },
            shared_root: String::new(),
            worker_name: String::new(),
            manifests: Vec::new(),
            manifest_input: String::new(),
            interval_seconds: 30,
            runner: TaskRunner::default(),
            control: None,
            stopping: false,
            notice: None,
            capacity: None,
            virtual_drive: false,
            bounded_shared: false,
            pool_sync: true,
            pool_retention: false,
            pool_history_limit: 0,
            pool_history_override: false,
            pool_status: None,
            shared_coordinator: false,
            shared_keep_previous: 0,
            cache_gib: 10,
            capacity_read: std::time::Instant::now(),
            identity_editor: crate::storage::admin::domains::DomainStore::load()
                .map(|s| {
                    s.remotes
                        .iter()
                        .map(|(remote, id)| format!("{} {} {}", remote, id.capacity, id.failure))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        }
    }
}

impl MountForm {
    pub(crate) fn is_running(&self) -> bool {
        self.runner.is_running()
    }

    pub(crate) fn poll(&mut self) {
        let terminal = self.runner.poll();
        if terminal.is_some() || self.capacity_read.elapsed() >= std::time::Duration::from_secs(1) {
            if let Some(control) = &self.control {
                self.pool_status = std::fs::read(control.path().join("pool-sync-status.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                self.capacity = std::fs::read(control.path().join("capacity.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            }
            self.capacity_read = std::time::Instant::now();
        }
        if let Some(status) = terminal {
            self.notice = Some(match status {
                JobStatus::Completed => "Mount/sync process finished. Check the log for writeback results; local workspace and VFS cache are retained.".into(),
                JobStatus::Cancelled => "Process force-stopped. Local files and VFS cache are retained; restart the same workspace to recover pending changes.".into(),
                _ => "Mount/sync failed. Check the log; local files and cache are retained. Windows mounts require WinFsp.".into(),
            });
            self.stopping = false;
            self.control = None;
        }
    }

    fn request_stop(&mut self) -> Result<(), String> {
        let control = self
            .control
            .as_ref()
            .ok_or("No mount control directory is available")?;
        std::fs::write(control.path().join("stop"), b"stop\n")
            .map_err(|error| format!("Could not request unmount: {error}"))?;
        self.stopping = true;
        Ok(())
    }

    fn start(&mut self, rclone: &str, sync_only: bool) -> Result<(), String> {
        self.start_action(rclone, if sync_only { 1 } else { 0 })
    }

    fn start_action(&mut self, rclone: &str, action: u8) -> Result<(), String> {
        let sync_only = action != 0;
        let automatic = self.virtual_drive && self.pool_sync;
        if self.pool.trim().is_empty() || self.workspace.trim().is_empty() {
            return Err("Select an upload pool and a persistent local workspace.".into());
        }
        if !automatic && self.shared_root.trim().is_empty() != self.worker_name.trim().is_empty() {
            return Err("Enter both a shared root and a worker name, or leave both empty for local-only mode.".into());
        }
        if !automatic
            && self.bounded_shared
            && (!self.virtual_drive || self.shared_root.trim().is_empty())
        {
            return Err(
                "Bounded shared history requires virtual mode and a shared encrypted root.".into(),
            );
        }
        if !sync_only && self.mountpoint.trim().is_empty() {
            return Err(
                "Enter an unused Windows drive letter or an existing empty Unix mount directory."
                    .into(),
            );
        }
        if !Path::new(self.workspace.trim()).is_absolute() {
            return Err("The persistent workspace must use an absolute path.".into());
        }
        let control = tempfile::Builder::new()
            .prefix("rpool-mount-control-")
            .tempdir()
            .map_err(|error| format!("Cannot create mount control directory: {error}"))?;
        let mut args = build_args(
            self.pool.trim(),
            Path::new(self.workspace.trim()),
            self.mountpoint.trim(),
            if automatic {
                ""
            } else {
                self.shared_root.trim()
            },
            if automatic {
                ""
            } else {
                self.worker_name.trim()
            },
            &self.manifests,
            self.interval_seconds,
            &control.path().join("stop"),
            sync_only,
        );
        if self.virtual_drive {
            args.push("--virtual-drive".into());
            args.push(format!("--cache-gib={}", self.cache_gib).into());
            if automatic {
                args.push("--pool-sync".into());
                if self.pool_retention {
                    args.push("--pool-retention".into());
                }
                if !self.worker_name.trim().is_empty() {
                    args.push(format!("--pool-worker={}", self.worker_name.trim()).into());
                }
                if self.pool_history_override || self.pool_retention {
                    args.push(format!("--pool-history-limit={}", self.pool_history_limit).into());
                }
            } else if self.bounded_shared {
                args.push("--bounded-shared".into());
                args.push(format!("--shared-keep-previous={}", self.shared_keep_previous).into());
                if self.shared_coordinator {
                    args.push("--shared-coordinator".into());
                }
            }
        }
        args.push("--status-file".into());
        args.push(control.path().join("capacity.json").into_os_string());
        if action >= 2 {
            args.retain(|arg| arg != "--sync-only");
            args.push(
                match action {
                    2 => "--capacity-only",
                    4 => "--cleanup-cache",
                    5 => "--recover-spool",
                    _ => "--migrate-excluded",
                }
                .into(),
            );
        }
        self.capacity = None;
        self.pool_status = None;
        self.runner.start_rpool(
            if action == 2 {
                "Check pool capacity"
            } else if action == 3 {
                "Migrate active archives (retain originals)"
            } else if action == 4 {
                "Trim clean shard cache"
            } else if action == 5 {
                "Export recoverable spool"
            } else if sync_only {
                "Sync local workspace"
            } else {
                "Mount workspace"
            },
            rclone,
            args,
        )?;
        self.control = Some(control);
        self.stopping = false;
        self.notice = Some(if action >= 2 { "Maintenance running. See log for capacity, cleanup or recovery results." } else if sync_only { "Synchronizing local workspace. See log for verified archive results." } else {
            "Mount process started; this is not yet proof that the drive is mounted or cloud changes are committed. See log for readiness and sync status."
        }.into());
        Ok(())
    }
}

impl Drop for MountForm {
    fn drop(&mut self) {
        if self.runner.is_running() {
            let _ = self.request_stop();
            // Child may still be importing or uploading when the window closes.
            // Preserve the sentinel until the child observes it; never cancel forcibly.
            if let Some(control) = self.control.take() {
                let _ = control.keep();
            }
        }
    }
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    egui::ScrollArea::vertical()
        .id_salt("mount-page")
        .show(ui, |ui| show_inner(ui, state));
}

fn show_inner(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    ui.heading("Mount a writable workspace");
    ui.label("Use Explorer/Finder to add, edit and delete files. Changes are archived to the selected pool in the background.");
    if !form.virtual_drive {
        ui.group(|ui| {
        ui.label("This keeps a complete plaintext local copy, plus a persistent VFS cache. Reserve enough disk space and protect the local workspace.");
        ui.label("Leave shared root and worker name empty for local-only mode. Existing archives are imported only from manifests you explicitly select.");
        ui.label("Shared mode uses eventual synchronization, not file locking. Use the same encrypted shared root on every PC and a distinct worker name. Conflicting edits create conflict copies to preserve both versions.");
        ui.label("While mounted, local changes are published and shared metadata is fetched, but incoming files do not replace local files live. Incoming changes and shared deletions are applied only during safe unmounted reconciliation or next start, with an empty persistent VFS cache.");
        ui.label("If cached writes remain, recover them through the original mount before synchronizing. The local copy and persistent cache are retained.");
        ui.label("In local-only mode, deletion affects only this workspace; shared deletions propagate during safe reconciliation. Previous remote archives are retained. Pending edits may remain local or in the VFS cache until the same workspace is restarted.");
    });
    } else {
        ui.label("Metadata-first virtual mode: verified shards are fetched on demand. Saves retain local spool/cache until cloud verification. Legacy mode preserves incoming revision copies. Bounded mode uses latest cloud state and expires old versions.");
    }
    let input_identity = (
        form.pool.clone(),
        form.workspace.clone(),
        form.shared_root.clone(),
        form.manifests.clone(),
        form.virtual_drive,
        form.pool_sync,
        form.pool_retention,
        form.bounded_shared,
        form.shared_coordinator,
        form.shared_keep_previous,
    );
    ui.add_enabled_ui(!form.runner.is_running(), |ui| {
        ui.horizontal(|ui| {
            ui.label("Upload pool");
            egui::ComboBox::from_id_salt("mount-pool")
                .selected_text(if form.pool.is_empty() {
                    "Select pool"
                } else {
                    &form.pool
                })
                .show_ui(ui, |ui| {
                    for name in &state.pool_names {
                        ui.selectable_value(&mut form.pool, name.clone(), name);
                    }
                });
        });
        ui.checkbox(&mut form.virtual_drive,"Virtual cloud drive — experimental (NEW workspace; lazy verified shards)");
        if form.virtual_drive {
            ui.checkbox(&mut form.pool_sync, "Automatic pool sync — no coordinator (NEW workspace)");
            if form.pool_sync {
                ui.label("RPool stores immutable sync metadata in every encrypted pool destination. No shared-root entry or dedicated PC. Use the same named pool and remote mapping on each PC.");
                ui.checkbox(&mut form.pool_retention, "Automatic history deletion — v7 (NEW workspace)");
                if !form.pool_retention { ui.checkbox(&mut form.pool_history_override, "Save future history limit in workspace config"); }
                if form.pool_history_override || form.pool_retention {
                    ui.horizontal(|ui| { ui.label("Previous versions per file"); ui.add(egui::DragValue::new(&mut form.pool_history_limit).range(0..=10000)); });
                }
                ui.colored_label(egui::Color32::YELLOW, if form.pool_retention { "V7 keeps current + selected history and unresolved conflicts. Collection copies retained data first and needs temporary space. All PCs must use the same limit. Legacy data is untouched; use a new workspace." } else { "V6 history limit is config-only: no automatic deletion. All metadata replicas must be reachable." });
            } else {
                ui.checkbox(&mut form.bounded_shared, "Legacy bounded shared mode — designated coordinator");
            }
            if form.bounded_shared && !form.pool_sync {
                ui.checkbox(&mut form.shared_coordinator, "This is the ONE coordinator PC");
                ui.horizontal(|ui| { ui.label("Previous versions (0 = latest only)"); ui.add(egui::DragValue::new(&mut form.shared_keep_previous).range(0..=100)); });
                ui.label("Cloud is authoritative; connect successfully before mounting. Latest only by default; extra history is optional. Deleted files lose their history. Offline PCs do not pin old cloud data. Unsynced stale writes and previous native cache are isolated locally. Keep exactly one coordinator workspace; other PCs wait for its acknowledgement.");
            }
            if !form.bounded_shared && !form.pool_sync {
                ui.label("Legacy virtual namespace uses shared-root/virtual-v3. Served files stay pinned for this mount; incoming changes appear as named revision copies. Dirty writes and history are retained. Empty directories currently remain local.");
            }
            ui.horizontal(|ui| {ui.label("Clean shard cache budget (GiB)");ui.add(egui::DragValue::new(&mut form.cache_gib).range(1..=1048576));});
        }
        directory_field(ui, "Persistent local workspace", &mut form.workspace);
        if !(form.virtual_drive && form.pool_sync) {
            ui.label("Shared encrypted root (optional, e.g. crypt:teamspace)");
            ui.text_edit_singleline(&mut form.shared_root);
        }
        ui.label(if form.virtual_drive && form.pool_sync { "Worker name (optional; generated per PC if empty)" } else { "Worker name (required with shared root; used in conflict filenames)" });
        ui.text_edit_singleline(&mut form.worker_name);
        ui.label(if cfg!(windows) {
            "Unused drive letter (requires WinFsp), e.g. R:"
        } else {
            "Existing empty mount directory (requires FUSE)"
        });
        ui.text_edit_singleline(&mut form.mountpoint);
        ui.horizontal(|ui| {
            ui.label("Background scan interval (seconds)");
            ui.add(egui::DragValue::new(&mut form.interval_seconds).range(2..=86400));
        });
        ui.separator();
        ui.label("Optional explicit archive imports (no pool membership is inferred)");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut form.manifest_input);
            if ui.button("Add manifest reference").clicked()
                && !form.manifest_input.trim().is_empty()
            {
                let value = form.manifest_input.trim().to_string();
                if !form.manifests.contains(&value) {
                    form.manifests.push(value);
                }
                form.manifest_input.clear();
            }
            if ui.button("Browse manifests…").clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Manifest JSON", &["json"])
                    .pick_files()
                {
                    for path in paths {
                        let value = path.display().to_string();
                        if !form.manifests.contains(&value) {
                            form.manifests.push(value);
                        }
                    }
                }
            }
        });
        let mut remove = None;
        egui::ScrollArea::vertical()
            .id_salt("mount-imports")
            .max_height(100.0)
            .show(ui, |ui| {
                for (index, source) in form.manifests.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(source);
                        if ui.small_button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                }
            });
        if let Some(index) = remove {
            form.manifests.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Mount read/write").clicked() {
                if let Err(error) = form.start(&state.settings.rclone, false) {
                    form.notice = Some(error);
                }
            }
            if form.virtual_drive && ui.button("Trim clean cache (retain dirty/history)").clicked() {
                if let Err(error)=form.start_action(&state.settings.rclone,4){form.notice=Some(error);}
            }
            if form.virtual_drive && ui.button("Export recoverable spool (unmounted)").clicked() {
                if let Err(error) = form.start_action(&state.settings.rclone, 5) { form.notice = Some(error); }
            }
            if ui.button("Refresh capacity / check exclusions").clicked() {
                if let Err(error) = form.start_action(&state.settings.rclone, 2) {
                    form.notice = Some(error);
                }
            }
            if ui.button("Sync without mounting").clicked() {
                if let Err(error) = form.start(&state.settings.rclone, true) {
                    form.notice = Some(error);
                }
            }
        });
    });
    if input_identity
        != (
            form.pool.clone(),
            form.workspace.clone(),
            form.shared_root.clone(),
            form.manifests.clone(),
            form.virtual_drive,
            form.pool_sync,
            form.pool_retention,
            form.bounded_shared,
            form.shared_coordinator,
            form.shared_keep_previous,
        )
    {
        form.capacity = None;
        form.pool_status = None;
    }
    if let Some(status) = &form.pool_status {
        ui.group(|ui| {
            ui.heading(format!("Pool sync · {} conflict groups", status.conflicts.len()));
            if !form.runner.is_running() { ui.small("Last sync snapshot (not a live cloud view)"); }
            ui.small(format!("{} automatic metadata destinations · previous-version limit: {} · deletion enabled: {}", status.roots.len(), status.desired_history_limit, status.history_deletion_enabled));
            for root in &status.roots { ui.small(root); }
            if status.conflicts.is_empty() { ui.label("No conflicts in the last synchronized metadata."); }
            for conflict in &status.conflicts {
                ui.collapsing(&conflict.path, |ui| {
                    for original in &conflict.originals {
                        ui.horizontal(|ui| { ui.label(format!("Original: {original}")); if ui.small_button("Copy path").clicked() { ui.ctx().copy_text(original.clone()); } });
                    }
                    if conflict.original_unavailable { ui.label("No common content original (for example, concurrent creation)."); }
                    if conflict.ambiguous_original { ui.label("Multiple common originals — manual review required."); }
                    for branch in &conflict.branches {
                        ui.horizontal(|ui| {
                            ui.label(format!("{} · {}", branch.worker, branch.file.as_deref().unwrap_or("Deletion request")));
                            if let Some(path) = &branch.file { if ui.small_button("Copy path").clicked() { ui.ctx().copy_text(path.clone()); } }
                        });
                    }
                    ui.small("All variants are preserved. Automatic merge/resolution is not enabled; no branch is discarded by viewing this list.");
                });
            }
        });
    }
    ui.collapsing("Account capacity / outage identities", |ui| {
        ui.label("One line: backing-remote account-budget-id outage-group-id. Use the SAME budget ID for aliases/accounts sharing quota; distinct budget IDs assert independent capacity. Outage groups are separate: accounts on one provider may fail together. No passwords or tokens.");
        ui.add_enabled_ui(!form.runner.is_running(), |ui| {
            ui.text_edit_multiline(&mut form.identity_editor);
            if ui.button("Save account identities").clicked() {
                let result: anyhow::Result<()> = (|| {
                    let mut store = crate::storage::admin::domains::DomainStore::default();
                    for line in form.identity_editor.lines().filter(|line| !line.trim().is_empty()) {
                        let words: Vec<_> = line.split_whitespace().collect();
                        if !(2..=3).contains(&words.len()) { anyhow::bail!("Use two or three fields per line"); }
                        if store.remotes.insert(words[0].into(), crate::storage::admin::domains::DomainIdentity {
                            capacity: words[1].into(), failure: words.get(2).copied().unwrap_or("").into()
                        }).is_some() { anyhow::bail!("Duplicate backing remote"); }
                    }
                    store.save()
                })();
                form.notice = Some(match result { Ok(()) => {form.capacity=None; "Identity declarations saved; refresh capacity.".into()}, Err(e)=>format!("{e:#}") });
            }
        });
    });
    if let Some(capacity) = &form.capacity {
        let mut migrate = false;
        ui.group(|ui| {
            ui.heading("Cloud pool capacity (estimate)");
            ui.label(format!("{} (without parity): {:.2} GiB / estimated usable ceiling: {:.2} GiB",
                if capacity.usage_scope == "shared-namespace" {"Known shared files + pending"} else {"Current local files"}, capacity.logical_used as f64 / 1073741824.0, capacity.logical_ceiling_estimate as f64 / 1073741824.0));
            let age = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(capacity.observed_unix)).unwrap_or(0);
            ui.label(format!("Additional full-group capacity: {:.2} GiB · eligible targets: {} · snapshot: {} seconds ago",
                capacity.additional_estimate as f64 / 1073741824.0, capacity.eligible.len(), age));
            ui.small(&capacity.note);
            for target in &capacity.targets {
                ui.small(format!("{} → {} · quota group {} · free {:.2} / total {:.2} GiB · {}",
                    target.remote, target.backing, target.capacity_domain,
                    target.free as f64 / 1073741824.0, target.total as f64 / 1073741824.0,
                    if target.declared { "user-declared identity" } else { "unverified overlap" }));
            }
            if let Some(committed) = capacity.committed_logical_used { ui.small(format!("Known committed shared namespace: {:.2} GiB (data only)", committed as f64 / 1073741824.0)); }
            ui.small("Usage excludes parity and may exclude writes still in VFS cache. Retained cloud history consumes physical quota. Replica mode reports local disk space; virtual mode serves this estimate through DAV quota.");
            for excluded in &capacity.excluded {
                ui.colored_label(egui::Color32::YELLOW, format!("{}: {}", excluded.remote, excluded.reason));
            }
            if capacity.retained_archives > 0 {
                ui.colored_label(egui::Color32::YELLOW, format!("{} active archives and {} locally known archive manifests reference excluded targets.", capacity.affected_active, capacity.retained_archives));
                ui.label("Migration switches active references only after verified copying. Originals and historical/shared references are retained; this does not free space on the old storage. Unmount and drain VFS cache first.");
                migrate = ui.add_enabled(!form.virtual_drive && !form.runner.is_running() && capacity.affected_active > 0 && !capacity.eligible.is_empty(),
                    egui::Button::new("Migrate active archives — retain originals")).clicked();
            }
        });
        if migrate {
            if let Err(error) = form.start_action(&state.settings.rclone, 3) {
                form.notice = Some(error);
            }
        }
    } else {
        ui.label("Cloud capacity is not available yet. Refresh capacity to inspect eligible storage; local disk capacity is separate.");
    }
    if form.runner.is_running() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(if form.stopping { "Graceful shutdown requested; pending edits may remain local. Check the log for results." } else { "Mount/sync process running — normal application jobs remain available" });
            if ui.add_enabled(!form.stopping, egui::Button::new("Unmount / finish sync")).clicked() {
                if let Err(error) = form.request_stop() { form.notice = Some(error); }
            }
        });
        ui.collapsing("Recovery controls", |ui| {
            ui.label("Force stop can interrupt uploads and leave edits in the local VFS cache. Restart this same workspace to recover. Cache files are not deleted.");
            if ui.add_enabled(form.stopping, egui::Button::new("Force stop process (retain local cache)")).clicked() { form.runner.cancel(); }
            if !form.stopping { ui.small("Request graceful unmount first to enable force stop."); }
        });
    }
    if let Some(notice) = &form.notice {
        ui.label(notice);
    }
    ui.separator();
    ui.label("Mount and writeback log");
    egui::ScrollArea::vertical()
        .id_salt("mount-log")
        .max_height(220.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in form.runner.logs() {
                ui.monospace(&line.text);
            }
        });
}

fn directory_field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.text_edit_singleline(value);
        if ui.button("Choose folder…").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
    });
}

fn build_args(
    pool: &str,
    workspace: &Path,
    mountpoint: &str,
    shared_root: &str,
    worker_name: &str,
    manifests: &[String],
    interval: u64,
    stop: &Path,
    sync_only: bool,
) -> Vec<OsString> {
    let mut args = vec![
        "mount".into(),
        format!("--pool={pool}").into(),
        "--workspace".into(),
        workspace.as_os_str().into(),
        "--interval-seconds".into(),
        interval.to_string().into(),
        "--stop-file".into(),
        stop.as_os_str().into(),
    ];
    if !shared_root.is_empty() {
        args.push(format!("--shared-root={shared_root}").into());
    }
    if !worker_name.is_empty() {
        args.push(format!("--worker-name={worker_name}").into());
    }
    if sync_only {
        args.push("--sync-only".into());
    } else {
        args.extend([OsString::from("--mountpoint"), mountpoint.into()]);
    }
    for source in manifests {
        args.push(format!("--manifest={source}").into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;
    #[test]
    fn mount_arguments_roundtrip_and_sync_omits_mountpoint() {
        for (sync_only, shared) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut args = vec![OsString::from("rpool")];
            args.extend(build_args(
                "-pool 한 글",
                &PathBuf::from("/persistent workspace"),
                "R:",
                if shared {
                    "crypt:team space/한 글"
                } else {
                    ""
                },
                if shared { "-PC 한 글" } else { "" },
                &[
                    "-manifest 한 글.json".into(),
                    "crypt:path with spaces/manifest.json".into(),
                ],
                42,
                Path::new("/control dir/stop"),
                sync_only,
            ));
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            let Some(crate::cli::Commands::Mount(parsed)) = cli.command else {
                panic!("expected mount");
            };
            assert_eq!(parsed.pool, "-pool 한 글");
            assert_eq!(parsed.workspace, PathBuf::from("/persistent workspace"));
            assert_eq!(parsed.sync_only, sync_only);
            assert_eq!(
                parsed.shared_root.as_deref(),
                shared.then_some("crypt:team space/한 글")
            );
            assert_eq!(parsed.worker_name.as_deref(), shared.then_some("-PC 한 글"));
            assert_eq!(parsed.mountpoint, (!sync_only).then(|| PathBuf::from("R:")));
            assert_eq!(
                parsed.manifests,
                [
                    "-manifest 한 글.json",
                    "crypt:path with spaces/manifest.json"
                ]
            );
            assert_eq!(parsed.interval_seconds, 42);
            assert_eq!(parsed.stop_file, Some(PathBuf::from("/control dir/stop")));
        }
    }
}
