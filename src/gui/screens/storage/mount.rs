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
    vfs_cache_gib: u64,
    cache_min_free_gib: u64,
    spool_gib: u64,
    recovery_source: String,
    recovery_skip_remotes: String,
    recovery_reprocess_plan: String,
    recovering_accounts: bool,
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
            virtual_drive: true,
            bounded_shared: false,
            pool_sync: true,
            pool_retention: false,
            pool_history_limit: 0,
            pool_history_override: false,
            pool_status: None,
            shared_coordinator: false,
            shared_keep_previous: 0,
            cache_gib: 10,
            vfs_cache_gib: 10,
            cache_min_free_gib: 2,
            spool_gib: 64,
            recovery_source: String::new(),
            recovery_skip_remotes: String::new(),
            recovery_reprocess_plan: String::new(),
            recovering_accounts: false,
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
    pub(crate) fn use_reprocess_plan(&mut self, path: &Path) {
        self.recovery_reprocess_plan = path.display().to_string();
        self.notice = Some("Reprocess plan selected. Open Recover after account removal, choose the original workspace and a new destination pool/workspace. Only validated completed replacements will be reused.".into());
    }
    pub(crate) fn from_settings(settings: &crate::gui::settings::GuiSettings) -> Self {
        let cache = &settings.mount_cache;
        let mut form = Self::default();
        form.virtual_drive = cache.online_drive;
        form.cache_gib = cache.shard_gib;
        form.vfs_cache_gib = cache.native_gib;
        form.cache_min_free_gib = cache.min_free_gib;
        form.spool_gib = cache.spool_gib;
        form
    }

    fn cache_settings(&self) -> crate::gui::settings::MountCacheSettings {
        crate::gui::settings::MountCacheSettings {
            online_drive: self.virtual_drive,
            shard_gib: self.cache_gib,
            native_gib: self.vfs_cache_gib,
            min_free_gib: self.cache_min_free_gib,
            spool_gib: self.spool_gib,
        }
    }

    fn save_mount_settings(
        &self,
        settings: &mut crate::gui::settings::GuiSettings,
    ) -> Result<(), String> {
        let mut next = settings.clone();
        if self.pool.is_empty() {
            next.mount_cache = self.cache_settings();
        } else {
            next.mount_profiles
                .insert(self.pool.clone(), self.profile());
        }
        crate::gui::settings::save(&next)?;
        *settings = next;
        Ok(())
    }

    fn profile(&self) -> crate::gui::settings::MountProfile {
        crate::gui::settings::MountProfile {
            workspace: self.workspace.clone(),
            mountpoint: self.mountpoint.clone(),
            shared_root: self.shared_root.clone(),
            worker_name: self.worker_name.clone(),
            manifests: self.manifests.clone(),
            interval_seconds: self.interval_seconds,
            bounded_shared: self.bounded_shared,
            pool_sync: self.pool_sync,
            pool_retention: self.pool_retention,
            pool_history_limit: self.pool_history_limit,
            pool_history_override: self.pool_history_override,
            shared_coordinator: self.shared_coordinator,
            shared_keep_previous: self.shared_keep_previous,
            cache: self.cache_settings(),
        }
    }

    fn select_pool(&mut self, pool: String, settings: &mut crate::gui::settings::GuiSettings) {
        if pool == self.pool {
            return;
        }
        if !self.pool.is_empty() {
            settings
                .mount_profiles
                .insert(self.pool.clone(), self.profile());
        }
        let profile = settings
            .mount_profiles
            .get(&pool)
            .cloned()
            .unwrap_or_else(|| crate::gui::settings::MountProfile {
                cache: settings.mount_cache.clone(),
                ..Default::default()
            });
        self.pool = pool;
        self.workspace = profile.workspace;
        self.mountpoint = profile.mountpoint;
        self.shared_root = profile.shared_root;
        self.worker_name = profile.worker_name;
        self.manifests = profile.manifests;
        self.interval_seconds = profile.interval_seconds;
        self.bounded_shared = profile.bounded_shared;
        self.pool_sync = profile.pool_sync;
        self.pool_retention = profile.pool_retention;
        self.pool_history_limit = profile.pool_history_limit;
        self.pool_history_override = profile.pool_history_override;
        self.shared_coordinator = profile.shared_coordinator;
        self.shared_keep_previous = profile.shared_keep_previous;
        self.virtual_drive = profile.cache.online_drive;
        self.cache_gib = profile.cache.shard_gib;
        self.vfs_cache_gib = profile.cache.native_gib;
        self.cache_min_free_gib = profile.cache.min_free_gib;
        self.spool_gib = profile.cache.spool_gib;
        self.manifest_input.clear();
        self.recovery_source.clear();
        self.recovery_skip_remotes.clear();
        self.recovery_reprocess_plan.clear();
        self.capacity = None;
        self.pool_status = None;
        self.notice = None;
    }

    fn append_cache_args(&self, args: &mut Vec<OsString>) {
        args.push(format!("--vfs-cache-gib={}", self.vfs_cache_gib).into());
        args.push(format!("--cache-min-free-gib={}", self.cache_min_free_gib).into());
        if self.virtual_drive {
            args.push("--virtual-drive".into());
            args.push(format!("--spool-gib={}", self.spool_gib).into());
            args.push(format!("--cache-gib={}", self.cache_gib).into());
        }
    }

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
            self.notice = Some(if self.recovering_accounts {
                match status {
                    JobStatus::Completed => "Recovery copy completed for the locally known source view. Inspect the recovery report, then mount this destination normally for reading and new writes. Original workspace retained.".into(),
                    _ => "Recovery stopped or is incomplete. See log and destination recovery report; original data is retained. Resume with the same source/destination, or mount the destination to use already recovered files and save new files.".into(),
                }
            } else {
                match status {
                JobStatus::Completed => "Mount/sync process finished. Check the log for writeback results; local workspace and VFS cache are retained.".into(),
                JobStatus::Cancelled => "Process force-stopped. Local files and VFS cache are retained; restart the same workspace to recover pending changes.".into(),
                _ => "Mount/sync failed. Check the log; local files and cache are retained. Windows mounts require WinFsp.".into(),
            }
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

    fn recovery_args(&self, stop: &Path) -> Result<Vec<OsString>, String> {
        if !self.virtual_drive || !self.pool_sync || self.pool_retention {
            return Err("Recovery destination must use Online drive + Automatic pool sync, with automatic history deletion OFF.".into());
        }
        if self.pool.trim().is_empty()
            || !Path::new(self.workspace.trim()).is_absolute()
            || !Path::new(self.recovery_source.trim()).is_absolute()
        {
            return Err("Select a NEW differently named destination pool, an absolute destination workspace, and the original source workspace.".into());
        }
        let mut args: Vec<OsString> = vec![
            "mount".into(),
            format!("--pool={}", self.pool.trim()).into(),
            "--workspace".into(),
            self.workspace.trim().into(),
            "--account-recovery-from".into(),
            self.recovery_source.trim().into(),
            "--pool-sync".into(),
            "--stop-file".into(),
            stop.as_os_str().to_owned(),
        ];
        self.append_cache_args(&mut args);
        for remote in self
            .recovery_skip_remotes
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            args.push(format!("--recovery-skip-remote={remote}").into());
        }
        if !self.recovery_reprocess_plan.trim().is_empty() {
            args.push("--recovery-reprocess-plan".into());
            args.push(self.recovery_reprocess_plan.trim().into());
        }
        Ok(args)
    }

    fn start_account_recovery(&mut self, rclone: &str) -> Result<(), String> {
        let control = tempfile::Builder::new()
            .prefix("rpool-recovery-control-")
            .tempdir()
            .map_err(|e| format!("Cannot create recovery control directory: {e}"))?;
        let args = self.recovery_args(&control.path().join("stop"))?;
        self.runner
            .start_rpool("Recover into remaining-account pool", rclone, args)?;
        self.control = Some(control);
        self.recovering_accounts = true;
        self.stopping = false;
        self.capacity = None;
        self.pool_status = None;
        self.notice = Some("Copying the locally known file view into the new destination. This does not mount a drive or remove source data. Review unresolved files in the report before treating recovery as complete.".into());
        Ok(())
    }

    fn start_action(&mut self, rclone: &str, action: u8) -> Result<(), String> {
        let sync_only = action != 0;
        let automatic = self.virtual_drive && self.pool_sync;
        if action == 6 && (!automatic || !self.manifests.is_empty()) {
            return Err("Apply pool changes requires Online drive + Automatic pool sync and no explicit imports.".into());
        }
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
        self.append_cache_args(&mut args);
        if self.virtual_drive {
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
                    6 => "--apply-pool-changes",
                    _ => "--migrate-excluded",
                }
                .into(),
            );
        }
        if action == 6 && !self.recovery_reprocess_plan.trim().is_empty() {
            args.push("--recovery-reprocess-plan".into());
            args.push(self.recovery_reprocess_plan.trim().into());
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
            } else if action == 6 {
                "Apply pool changes (preserve original workspace)"
            } else if sync_only {
                "Sync local workspace"
            } else {
                "Mount workspace"
            },
            rclone,
            args,
        )?;
        self.control = Some(control);
        self.recovering_accounts = false;
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
        let mut selected_pool = form.pool.clone();
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
                        ui.selectable_value(&mut selected_pool, name.clone(), name);
                    }
                });
        });
        form.select_pool(selected_pool, &mut state.settings);
        ui.checkbox(&mut form.virtual_drive,"Online drive — on-demand files and automatic local cache cleanup (recommended)");
        if form.virtual_drive {
            ui.label("All known files remain visible; only needed contents are downloaded. Clean shards are evicted in least-recently-used order; unused OS cache also expires promptly. Cloud files are not deleted. Internet access is required for evicted contents.");
            ui.label("Use a NEW workspace when switching from a full replica. Existing replica files are not converted or deleted automatically. Native mount support remains experimental.");
        }
        if form.virtual_drive {
            ui.checkbox(&mut form.pool_sync, "Automatic pool sync — no coordinator");
            if form.pool_sync {
                ui.label("RPool stores immutable sync metadata in every encrypted pool destination. No shared-root entry or dedicated PC. Use the same named pool and remote mapping on each PC.");
                ui.checkbox(&mut form.pool_retention, "Automatic history deletion — v7");
                if !form.pool_retention { ui.checkbox(&mut form.pool_history_override, "Save future history limit in workspace config"); }
                if form.pool_history_override || form.pool_retention {
                    ui.horizontal(|ui| { ui.label("Previous versions per file"); ui.add(egui::DragValue::new(&mut form.pool_history_limit).range(0..=10000)); });
                }
                ui.colored_label(egui::Color32::YELLOW, if form.pool_retention { "V7 keeps current + selected history and unresolved conflicts. Collection needs temporary space. All PCs must use the same limit. Keep the saved mode for an existing workspace; changing v6/v7 protocol is separate from applying storage membership changes." } else { "V6 history limit is config-only: no automatic deletion. All metadata replicas must be reachable; use Apply pool changes below after adding/removing storage." });
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
        ui.horizontal(|ui| { ui.label("Native OS cache target (GiB)"); ui.add(egui::DragValue::new(&mut form.vfs_cache_gib).range(1..=1048576)); });
        ui.horizontal(|ui| { ui.label("Keep disk free (GiB, native cache target)"); ui.add(egui::DragValue::new(&mut form.cache_min_free_gib).range(0..=1048576)); });
        if form.virtual_drive {
            ui.horizontal(|ui| { ui.label("Pending write spool limit (GiB)"); ui.add(egui::DragValue::new(&mut form.spool_gib).range(1..=1048576)); });
            ui.label("Least-recently-used clean data is evicted and downloaded again when needed. Shard, OS cache and pending-write limits are separate and add together. A shard/recovery group must fit the shard budget.");
        } else {
            ui.label("Replica mode retains the complete local file copy; the OS cache target does not limit that copy. Use a NEW virtual workspace for on-demand downloads.");
        }
        ui.label("Open files and unsaved native writes are protected and may exceed the OS cache target. Recovery copies, metadata and upload staging need additional disk space. Cache settings apply on the next start.");
        if form.virtual_drive {
            ui.label(format!("Clean data cache targets combined: {} GiB. Pending writes: up to {} GiB separately (not preallocated).", form.cache_gib.saturating_add(form.vfs_cache_gib), form.spool_gib));
        } else {
            ui.label("The native cache target does not limit replica files or pending replica writes.");
        }
        if ui.button("Save mount settings").clicked() {
            form.notice = Some(match form.save_mount_settings(&mut state.settings) {
                Ok(()) => {
                    "Mount settings saved on this PC for the selected pool; select that pool to restore them after restart.".into()
                }
                Err(error) => error,
            });
        }
        directory_field(ui, "Persistent local workspace", &mut form.workspace);
        if form.virtual_drive && form.pool_sync {
            ui.collapsing("Apply changed pool to this workspace", |ui| {
                ui.label("Worker count, retries and shard/layout changes take effect on the next mount when storage membership is unchanged.");
                ui.label("After adding/removing storage accounts: stop the mount, then apply below. The selected pool name and workspace path stay the same. Current known files, conflict copies and sealed writes are verified in an independent metadata generation before activation.");
                ui.colored_label(egui::Color32::YELLOW, "Old history and native-cache recovery data remain in a sibling backup, not the active generation. Unseen changes on other PCs are not imported. Old PCs remain on the old metadata generation. This is a migration and can take time/extra storage; it does not erase/reset the workspace.");
                ui.label("Completed Reprocess plan (optional; used as verified input for independent new data)");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut form.recovery_reprocess_plan);
                    if ui.button("Choose plan…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Reprocess plan", &["json"]).pick_file() {
                            form.recovery_reprocess_plan = path.display().to_string();
                        }
                    }
                });
                if ui.button("Apply pool changes — keep workspace path").clicked() {
                    if let Err(error) = form.save_mount_settings(&mut state.settings)
                        .and_then(|()| form.start_action(&state.settings.rclone, 6)) {
                        form.notice = Some(error);
                    }
                }
                ui.small("When completed, use Mount read/write with the same path. Interrupted transitions resume with the same settings and this button.");
            });
        }
        ui.collapsing("Recover after account removal — copy to a new writable pool", |ui| {
            ui.label("1. In Pools, save remaining accounts under a NEW pool name. Select that destination above and a NEW empty workspace (or the same recovery destination to resume). Turn automatic history deletion OFF.");
            ui.label("2. Select the original online workspace below. Stop its mount first. Source data is retained; verified file contents are copied and uploaded independently, not just linked.");
            directory_field(ui, "Original source workspace", &mut form.recovery_source);
            ui.label("Unavailable rclone remote aliases to skip while reading (one per line, e.g. broken-crypt; no colon/path). Keep them out of the destination pool.");
            ui.text_edit_multiline(&mut form.recovery_skip_remotes);
            ui.label("Completed Reprocess plan (optional): reuse verified replacement archives without uploading the same contents again. Choose the plan.json shown by Reprocess data.");
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut form.recovery_reprocess_plan);
                if ui.button("Choose Reprocess plan…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Reprocess plan", &["json"]).pick_file() {
                        form.recovery_reprocess_plan = path.display().to_string();
                    }
                }
            });
            ui.label("Only locally known current files, pending sealed writes and conflict copies can be recovered. History, unseen peer changes and dirty native-cache writes are not silently discarded or declared recovered. Insufficient surviving data is reported per file.");
            if ui.button("Recover files into selected destination").clicked() {
                if let Err(error) = form.save_mount_settings(&mut state.settings)
                    .and_then(|()| form.start_account_recovery(&state.settings.rclone)) {
                    form.notice = Some(error);
                }
            }
            ui.label("3. Inspect the recovery report/log, then use Mount read/write above. The destination uses only its configured remaining accounts for new writes. Old workspaces are not converted in place.");
        });
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
                if let Err(error) = form.save_mount_settings(&mut state.settings)
                    .and_then(|()| form.start(&state.settings.rclone, false)) {
                    form.notice = Some(error);
                }
            }
            if form.virtual_drive && ui.button("Trim clean cache (retain dirty/history)").clicked() {
                if let Err(error)=form.save_mount_settings(&mut state.settings).and_then(|()| form.start_action(&state.settings.rclone, 4)){form.notice=Some(error);}
            }
            if form.virtual_drive && ui.button("Export recoverable spool (unmounted)").clicked() {
                if let Err(error) = form.save_mount_settings(&mut state.settings).and_then(|()| form.start_action(&state.settings.rclone, 5)) { form.notice = Some(error); }
            }
            if ui.button("Refresh capacity / check exclusions").clicked() {
                if let Err(error) = form.save_mount_settings(&mut state.settings).and_then(|()| form.start_action(&state.settings.rclone, 2)) {
                    form.notice = Some(error);
                }
            }
            if ui.button("Sync without mounting").clicked() {
                if let Err(error) = form.save_mount_settings(&mut state.settings)
                    .and_then(|()| form.start(&state.settings.rclone, true)) {
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
    egui::CollapsingHeader::new("Account capacity / outage identities")
        .default_open(true)
        .show(ui, |ui| {
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
                form.notice = Some(match result { Ok(()) => {
                    form.capacity=None;
                    state.pools.invalidate_capacity();
                    "Identity declarations saved; refresh capacity.".into()
                }, Err(e)=>format!("{e:#}") });
            }
        });
    });
    if let Some(capacity) = &form.capacity {
        let mut migrate = false;
        ui.group(|ui| {
            ui.heading("Cloud pool capacity");
            crate::gui::widgets::pool_capacity::summary(ui, capacity);
            ui.label(format!("{} (logical file bytes): {:.2} GiB / including the next-file estimate: {:.2} GiB",
                if capacity.usage_scope == "shared-namespace" {"Known shared files + pending"} else {"Current local files"}, capacity.logical_used as f64 / 1073741824.0, capacity.logical_ceiling_estimate as f64 / 1073741824.0));
            let age = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(capacity.observed_unix)).unwrap_or(0);
            ui.label(format!("Next-file estimate: {:.2} GiB · eligible targets: {} · snapshot: {} seconds ago",
                capacity.additional_estimate as f64 / 1073741824.0, capacity.eligible.len(), age));
            if age > 120 || capacity.eligible.is_empty() {
                ui.colored_label(egui::Color32::YELLOW, "Cloud capacity is unverified/stale. Explorer reports zero additional free space conservatively, not a real 1 PB drive. This is not proof that the pool is full; refresh capacity and check account errors.");
            }
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
            if let Err(error) = form
                .save_mount_settings(&mut state.settings)
                .and_then(|()| form.start_action(&state.settings.rclone, 3))
            {
                form.notice = Some(error);
            }
        }
    } else {
        ui.label("Cloud capacity is not available yet. Refresh capacity and check account errors. Online drives report zero additional free space until verified; this is not proof the pool is full. Local disk capacity is separate.");
    }
    if form.runner.is_running() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(if form.stopping { "Graceful shutdown requested; pending edits may remain local. Check the log for results." } else { "Mount/sync process running — normal application jobs remain available" });
            if ui.add_enabled(!form.stopping, egui::Button::new("Unmount / stop (retain pending data)")).clicked() {
                if let Err(error) = form.request_stop() { form.notice = Some(error); }
            }
        });
        if form.virtual_drive {
            ui.small("Online unmount retains pending local data; it does not wait for a full cloud upload. Use Sync without mounting to finish replication separately.");
        }
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
    #[test]
    fn pool_profiles_restore_all_options_and_isolate_new_pools() {
        let mut settings = crate::gui::settings::GuiSettings::default();
        settings.mount_cache.native_gib = 7;
        let mut form = super::MountForm::from_settings(&settings);
        form.select_pool("A".into(), &mut settings);
        form.workspace = "/persistent/A".into();
        form.mountpoint = "/mount/A".into();
        form.shared_root = "crypt:team".into();
        form.worker_name = "desktop".into();
        form.manifests = vec!["archive.json".into()];
        form.interval_seconds = 42;
        form.pool_retention = true;
        form.pool_history_limit = 3;
        form.pool_history_override = true;
        form.bounded_shared = true;
        form.shared_coordinator = true;
        form.shared_keep_previous = 2;
        form.cache_gib = 23;
        let a = form.profile();
        form.recovery_source = "source".into();
        form.recovery_skip_remotes = "old-remote".into();
        form.recovery_reprocess_plan = "plan.json".into();
        form.manifest_input = "unfinished".into();
        form.select_pool("B".into(), &mut settings);
        assert_eq!(
            form.profile(),
            crate::gui::settings::MountProfile {
                cache: settings.mount_cache.clone(),
                ..Default::default()
            }
        );
        assert!(form.recovery_source.is_empty());
        assert!(form.recovery_skip_remotes.is_empty());
        assert!(form.recovery_reprocess_plan.is_empty());
        assert!(form.manifest_input.is_empty());
        form.workspace = "/persistent/B".into();
        form.virtual_drive = false;
        form.pool_sync = false;
        let b = form.profile();
        form.select_pool("A".into(), &mut settings);
        assert_eq!(form.profile(), a);
        form.select_pool("B".into(), &mut settings);
        assert_eq!(form.profile(), b);
        // Simulate restarting with persisted settings, without writing real GUI config.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gui-settings.json");
        crate::utils::save_json_atomic(&path, &settings).unwrap();
        let mut restored = crate::utils::read_json(&path).unwrap();
        let mut restarted = super::MountForm::from_settings(&restored);
        restarted.select_pool("A".into(), &mut restored);
        assert_eq!(restarted.profile(), a);
        restarted.select_pool("B".into(), &mut restored);
        assert_eq!(restarted.profile(), b);
    }

    #[test]
    fn legacy_settings_seed_only_safe_cache_defaults_for_each_pool() {
        let mut settings: crate::gui::settings::GuiSettings =
            serde_json::from_str(r#"{"mount_cache":{"online_drive":false,"native_gib":5}}"#)
                .unwrap();
        assert!(settings.mount_profiles.is_empty());
        let mut form = super::MountForm::from_settings(&settings);
        form.select_pool("legacy".into(), &mut settings);
        assert!(!form.virtual_drive);
        assert_eq!(form.vfs_cache_gib, 5);
        assert!(form.workspace.is_empty());
        assert!(!form.pool_retention);
        let partial: crate::gui::settings::GuiSettings =
            serde_json::from_str(r#"{"mount_profiles":{"A":{"workspace":"/A"}}}"#).unwrap();
        let profile = &partial.mount_profiles["A"];
        assert_eq!(profile.workspace, "/A");
        assert!(!profile.pool_retention);
        assert!(profile.pool_sync && profile.cache.online_drive);
    }

    #[test]
    fn recovery_form_forwards_source_and_skip_aliases_without_mount_or_retention() {
        let mut form = super::MountForm::default();
        form.pool = "recovered-pool".into();
        let base = std::env::temp_dir();
        form.workspace = base.join("new destination 한 글").display().to_string();
        form.recovery_source = base.join("original source 한 글").display().to_string();
        form.recovery_skip_remotes = "badcrypt\n another crypt \n".into();
        form.recovery_reprocess_plan = base
            .join("reprocess operation/plan.json")
            .display()
            .to_string();
        let args = form.recovery_args(&base.join("stop")).unwrap();
        let parsed = crate::cli::Cli::try_parse_from(
            std::iter::once(std::ffi::OsString::from("rpool")).chain(args),
        )
        .unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount")
        };
        assert_eq!(
            args.account_recovery_from,
            Some(form.recovery_source.clone().into())
        );
        assert_eq!(args.recovery_skip_remote, ["badcrypt", "another crypt"]);
        assert_eq!(
            args.recovery_reprocess_plan,
            Some(form.recovery_reprocess_plan.clone().into())
        );
        assert!(args.virtual_drive && args.pool_sync);
        assert!(!args.pool_retention && args.mountpoint.is_none() && !args.sync_only);
        let mut invalid = super::MountForm::default();
        invalid.pool_retention = true;
        assert!(invalid.recovery_args(&base.join("stop")).is_err());
    }
    #[test]
    fn saved_cache_preferences_reach_mount_cli_and_keep_explicit_replica() {
        use clap::Parser;
        for online in [true, false] {
            let mut settings = crate::gui::settings::GuiSettings::default();
            settings.mount_cache = crate::gui::settings::MountCacheSettings {
                online_drive: online,
                shard_gib: 4,
                native_gib: 5,
                min_free_gib: 3,
                spool_gib: 8,
            };
            let restored = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
            let form = super::MountForm::from_settings(&restored);
            assert_eq!(form.cache_settings(), settings.mount_cache);
            let mut args: Vec<std::ffi::OsString> = [
                "rpool",
                "mount",
                "--pool=p",
                "--workspace=/new",
                "--sync-only",
            ]
            .into_iter()
            .map(Into::into)
            .collect();
            form.append_cache_args(&mut args);
            let parsed = crate::cli::Cli::try_parse_from(args).unwrap();
            let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
                panic!("mount")
            };
            assert_eq!(args.virtual_drive, online);
            assert_eq!((args.vfs_cache_gib, args.cache_min_free_gib), (5, 3));
            if online {
                assert_eq!((args.cache_gib, args.spool_gib), (4, 8));
            }
        }
        let form = super::MountForm::from_settings(&crate::gui::settings::GuiSettings::default());
        assert!(form.virtual_drive && form.pool_sync);
        assert!(form.workspace.is_empty());
    }
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
