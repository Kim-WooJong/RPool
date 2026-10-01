//! Mount screen state, persistence per pool, and the actions it starts.
use super::build_args;
use super::session::{MountSession, SessionSpec};
use crate::gui::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

pub(crate) struct MountForm {
    /// The selected pool's session (idle when that pool does not run).
    pub(super) session: MountSession,
    /// Sessions of other pools that ran when another pool was selected.
    pub(super) background: BTreeMap<String, MountSession>,
    pub(super) pool: String,
    pub(super) workspace: String,
    pub(super) mountpoint: String,
    pub(super) shared_root: String,
    pub(super) worker_name: String,
    pub(super) manifests: Vec<String>,
    pub(super) manifest_input: String,
    pub(super) interval_seconds: u64,
    pub(super) virtual_drive: bool,
    pub(super) bounded_shared: bool,
    pub(super) pool_sync: bool,
    pub(super) pool_retention: bool,
    pub(super) pool_history_limit: u32,
    pub(super) pool_history_override: bool,
    pub(super) shared_coordinator: bool,
    pub(super) shared_keep_previous: usize,
    pub(super) cache_gib: u64,
    pub(super) vfs_cache_gib: u64,
    pub(super) cache_min_free_gib: u64,
    pub(super) spool_gib: u64,
    pub(super) recovery_source: String,
    /// Plain rclone `remote:path` to import into the drive.
    pub(super) import_source: String,
    /// Drive folder the import lands in; empty for the root.
    pub(super) import_destination: String,
    pub(super) import_batch_gib: u64,
    pub(super) import_rename: bool,
    /// Selected Drive page tab.
    pub(crate) tab: crate::gui::state::DriveTab,
    pub(super) recovery_skip_remotes: String,
    pub(super) recovery_reprocess_plan: String,
    pub(super) keep_previous: usize,
    pub(super) diagnostic_read_only: bool,
    pub(super) retention_confirmed: bool,
    pub(super) frontend: crate::cli::Frontend,
    pub(super) native_read_only: bool,
}

/// Pseudo action for [`MountForm::spec_for`]: account recovery.
pub(super) const RECOVERY: u8 = u8::MAX;

impl Default for MountForm {
    fn default() -> Self {
        Self {
            session: MountSession::default(),
            background: BTreeMap::new(),
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
            virtual_drive: true,
            bounded_shared: false,
            pool_sync: true,
            pool_retention: false,
            pool_history_limit: 0,
            pool_history_override: false,
            shared_coordinator: false,
            shared_keep_previous: 0,
            cache_gib: 10,
            vfs_cache_gib: 10,
            cache_min_free_gib: 2,
            spool_gib: 64,
            recovery_source: String::new(),
            import_source: String::new(),
            import_destination: String::new(),
            import_batch_gib: 4,
            import_rename: false,
            tab: crate::gui::state::DriveTab::default(),
            recovery_skip_remotes: String::new(),
            recovery_reprocess_plan: String::new(),
            keep_previous: 3,
            diagnostic_read_only: false,
            retention_confirmed: false,
            frontend: Default::default(),
            native_read_only: false,
        }
    }
}

impl MountForm {
    pub(crate) fn use_reprocess_plan(&mut self, path: &Path) {
        self.recovery_reprocess_plan = path.display().to_string();
        self.session.notice = Some(tr("Reprocess plan selected. Open Recover after account removal, choose the original workspace and a new destination pool/workspace. Only validated completed replacements will be reused.").into());
    }
    pub(crate) fn from_settings(settings: &crate::gui::settings::GuiSettings) -> Self {
        let cache = &settings.mount_cache;
        Self {
            virtual_drive: cache.online_drive,
            cache_gib: cache.shard_gib,
            vfs_cache_gib: cache.native_gib,
            cache_min_free_gib: cache.min_free_gib,
            spool_gib: cache.spool_gib,
            ..Self::default()
        }
    }

    pub(super) fn cache_settings(&self) -> crate::gui::settings::MountCacheSettings {
        crate::gui::settings::MountCacheSettings {
            online_drive: self.virtual_drive,
            shard_gib: self.cache_gib,
            native_gib: self.vfs_cache_gib,
            min_free_gib: self.cache_min_free_gib,
            spool_gib: self.spool_gib,
        }
    }

    pub(super) fn save_mount_settings(
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

    pub(super) fn profile(&self) -> crate::gui::settings::MountProfile {
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
            frontend: self.frontend,
            native_read_only: self.native_read_only,
            keep_previous: self.keep_previous,
        }
    }

    pub(crate) fn select_pool(
        &mut self,
        pool: String,
        settings: &mut crate::gui::settings::GuiSettings,
    ) {
        if pool == self.pool {
            return;
        }
        if !self.pool.is_empty() {
            settings
                .mount_profiles
                .insert(self.pool.clone(), self.profile());
        }
        let saved = settings.mount_profiles.get(&pool).cloned();
        let fresh = saved.is_none();
        let profile = saved.unwrap_or_else(|| crate::gui::settings::MountProfile {
            cache: settings.mount_cache.clone(),
            ..Default::default()
        });
        // Keep a running session; an idle one only holds the last result.
        let previous = std::mem::replace(
            &mut self.session,
            self.background.remove(&pool).unwrap_or_default(),
        );
        if previous.is_running() {
            self.background.insert(self.pool.clone(), previous);
        }
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
        self.frontend = profile.frontend;
        self.native_read_only = profile.native_read_only;
        self.keep_previous = profile.keep_previous;
        self.retention_confirmed = false;
        self.virtual_drive = profile.cache.online_drive;
        self.cache_gib = profile.cache.shard_gib;
        self.vfs_cache_gib = profile.cache.native_gib;
        self.cache_min_free_gib = profile.cache.min_free_gib;
        self.spool_gib = profile.cache.spool_gib;
        self.manifest_input.clear();
        self.recovery_source.clear();
        self.recovery_skip_remotes.clear();
        self.recovery_reprocess_plan.clear();
        if fresh {
            self.avoid_taken_drive_letter();
        }
    }

    /// A native frontend needs a local online drive: no pool sync or shared root.
    pub(super) fn native_allowed(&self) -> bool {
        self.virtual_drive && !self.bounded_shared && self.shared_root.trim().is_empty()
    }
    /// Whether this mount will use a native frontend on this build.
    pub(super) fn native_selected(&self) -> bool {
        use crate::cli::Frontend;
        self.native_allowed()
            && Frontend::native_here().is_some()
            && matches!(
                self.frontend,
                Frontend::Auto | Frontend::Fuse | Frontend::Winfsp
            )
    }
    /// `--frontend` for a mount (never for sync or maintenance actions). Always
    /// explicit, so the CLI's `auto` default never picks a frontend the form
    /// did not offer.
    pub(super) fn frontend_args(&self, sync_only: bool) -> Vec<OsString> {
        use crate::cli::Frontend;
        if sync_only {
            return vec![];
        }
        let frontend = if self.native_allowed() {
            self.frontend
        } else {
            Frontend::Dav
        };
        let mut args = vec![OsString::from(format!(
            "--frontend={}",
            frontend.cli_value()
        ))];
        if self.native_read_only && self.native_selected() {
            args.push("--native-read-only".into());
        }
        args
    }

    pub(super) fn append_cache_args(&self, args: &mut Vec<OsString>) {
        args.push(format!("--vfs-cache-gib={}", self.vfs_cache_gib).into());
        args.push(format!("--cache-min-free-gib={}", self.cache_min_free_gib).into());
        if self.virtual_drive {
            args.push("--virtual-drive".into());
            args.push(format!("--spool-gib={}", self.spool_gib).into());
            args.push(format!("--cache-gib={}", self.cache_gib).into());
        }
    }

    /// Forget the shown capacity (for example after identities changed).
    pub(crate) fn invalidate_capacity(&mut self) {
        self.session.capacity = None;
        for session in self.background.values_mut() {
            session.capacity = None;
        }
    }

    /// Whether the selected pool's session runs.
    pub(crate) fn is_running(&self) -> bool {
        self.session.is_running()
    }

    /// Polls every session; the selected one reads the edited workspace.
    pub(crate) fn poll(&mut self) {
        self.session.poll(&self.workspace);
        for session in self.background.values_mut() {
            let workspace = session
                .spec
                .as_ref()
                .map(|s| s.workspace.clone())
                .unwrap_or_default();
            session.poll(&workspace);
        }
    }

    pub(super) fn request_stop(&mut self) -> Result<(), String> {
        self.session.request_stop()
    }

    /// What `action` would start with now; `RECOVERY` for account recovery.
    pub(super) fn spec_for(&self, action: u8) -> SessionSpec {
        let mount = action == 0;
        SessionSpec {
            pool: self.pool.trim().into(),
            workspace: self.workspace.trim().into(),
            mountpoint: if mount {
                self.mountpoint.trim().into()
            } else {
                String::new()
            },
            frontend: mount.then(|| {
                if self.native_selected() {
                    crate::cli::Frontend::native_here().unwrap_or(crate::cli::Frontend::Dav)
                } else {
                    crate::cli::Frontend::Dav
                }
            }),
            reads: if action == RECOVERY && !self.recovery_source.trim().is_empty() {
                vec![self.recovery_source.trim().into()]
            } else {
                Vec::new()
            },
            keep_previous: self.keep_previous,
        }
    }

    fn check_conflict(&self, action: u8) -> Result<(), String> {
        match self.conflict(action) {
            Some(conflict) => Err(conflict.message()),
            None => Ok(()),
        }
    }

    pub(super) fn start(&mut self, rclone: &str, sync_only: bool) -> Result<(), String> {
        self.start_action(rclone, if sync_only { 1 } else { 0 })
    }

    pub(super) fn recovery_args(&self, stop: &Path) -> Result<Vec<OsString>, String> {
        if !self.virtual_drive || !self.pool_sync || self.pool_retention {
            return Err(tr("Recovery destination must use Online drive + Automatic pool sync, with automatic history deletion OFF.").into());
        }
        if self.pool.trim().is_empty()
            || !Path::new(self.workspace.trim()).is_absolute()
            || !Path::new(self.recovery_source.trim()).is_absolute()
        {
            return Err(tr("Select a NEW differently named destination pool, an absolute destination workspace, and the original source workspace.").into());
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

    pub(super) fn start_account_recovery(&mut self, rclone: &str) -> Result<(), String> {
        let control = tempfile::Builder::new()
            .prefix("rpool-recovery-control-")
            .tempdir()
            .map_err(|e| {
                trf(
                    "Cannot create recovery control directory: {error}",
                    &[("error", &e)],
                )
            })?;
        let args = self.recovery_args(&control.path().join("stop"))?;
        self.check_conflict(RECOVERY)?;
        let spec = self.spec_for(RECOVERY);
        let session = &mut self.session;
        session
            .runner
            .start_rpool("Recover into remaining-account pool", rclone, args)?;
        session.started(control, spec, 0);
        session.recovering_accounts = true;
        session.capacity = None;
        session.pool_status = None;
        session.notice = Some(tr("Copying the locally known file view into the new destination. This does not mount a drive or remove source data. Review unresolved files in the report before treating recovery as complete.").into());
        Ok(())
    }

    /// Local online drives only: retention and native frontends conflict with
    /// pool sync and shared roots.
    pub(super) fn retention_allowed(&self) -> bool {
        self.virtual_drive
            && !self.pool_sync
            && !self.bounded_shared
            && self.shared_root.trim().is_empty()
    }
    pub(super) fn retention_key(&self) -> (String, String, usize) {
        (
            self.pool.trim().into(),
            self.workspace.trim().into(),
            self.keep_previous,
        )
    }
    /// Deleting obsolete versions needs a successful preview of the same
    /// pool, workspace and history limit, plus confirmed exclusive ownership.
    pub(super) fn retention_ready(&self) -> bool {
        self.retention_allowed()
            && self.retention_confirmed
            && self.session.retention_previewed.as_ref() == Some(&self.retention_key())
    }

    /// Validated `rpool mount` arguments for `action`: 0 mount, 1 sync, 2
    /// capacity, 3 migrate, 4 trim cache, 5 export spool, 6 apply pool
    /// changes, 7 retention preview, 8 apply retention, 9 import from rclone.
    pub(super) fn action_args(&self, action: u8, control: &Path) -> Result<Vec<OsString>, String> {
        let sync_only = action != 0;
        let automatic = self.virtual_drive && self.pool_sync;
        if action == 6 && (!automatic || !self.manifests.is_empty()) {
            return Err(tr("Apply pool changes requires Online drive + Automatic pool sync and no explicit imports.").into());
        }
        if matches!(action, 7 | 8) && !self.retention_allowed() {
            return Err(tr("History cleanup needs an online drive with Sync = This PC only (no pool sync or shared root).").into());
        }
        if action == 9 {
            if !self.virtual_drive
                || self.bounded_shared
                || (!automatic && !self.shared_root.trim().is_empty())
            {
                return Err(tr(
                    "Importing needs an online drive with This PC only or Automatic pool sync.",
                )
                .into());
            }
            if !self.import_source.trim().contains(':') {
                return Err(tr(
                    "Enter the rclone source as remote:path (for example old-crypt:photos).",
                )
                .into());
            }
        }
        if action == 8 && !self.retention_ready() {
            return Err(tr("Preview obsolete versions for this pool, workspace and limit, and confirm exclusive ownership, before deleting.").into());
        }
        let diagnostic =
            action == 0 && automatic && self.pool_retention && self.diagnostic_read_only;
        if diagnostic && !self.manifests.is_empty() {
            return Err(tr(
                "A diagnostic read-only mount cannot import archives; remove the manifest imports.",
            )
            .into());
        }
        if self.pool.trim().is_empty() || self.workspace.trim().is_empty() {
            return Err(tr("Select an upload pool and a persistent local workspace.").into());
        }
        if !automatic && self.shared_root.trim().is_empty() != self.worker_name.trim().is_empty() {
            return Err(tr("Enter both a shared root and a worker name, or leave both empty for local-only mode.").into());
        }
        if !automatic
            && self.bounded_shared
            && (!self.virtual_drive || self.shared_root.trim().is_empty())
        {
            return Err(tr(
                "Bounded shared history requires virtual mode and a shared encrypted root.",
            )
            .into());
        }
        if !sync_only && self.mountpoint.trim().is_empty() {
            return Err(tr(
                "Enter an unused Windows drive letter or an existing empty Unix mount directory.",
            )
            .into());
        }
        if !Path::new(self.workspace.trim()).is_absolute() {
            return Err(tr("The persistent workspace must use an absolute path.").into());
        }
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
            // Listed archives are applied at the next mount; an rclone import
            // is its own offline action and must not carry them.
            if action == 9 { &[] } else { &self.manifests },
            self.interval_seconds,
            &control.join("stop"),
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
        if diagnostic {
            args.push("--diagnostic-read-only".into());
        }
        args.extend(self.frontend_args(sync_only));
        args.push("--status-file".into());
        args.push(control.join("capacity.json").into_os_string());
        if action >= 2 {
            args.retain(|arg| arg != "--sync-only");
            args.push(
                match action {
                    2 => "--capacity-only",
                    4 => "--cleanup-cache",
                    5 => "--recover-spool",
                    6 => "--apply-pool-changes",
                    7 => "--retention-report",
                    8 => "--apply-retention",
                    9 => "--import-from",
                    _ => "--migrate-excluded",
                }
                .into(),
            );
        }
        if action == 9 {
            args.push(self.import_source.trim().into());
            let destination = self.import_destination.trim().trim_matches('/');
            if !destination.is_empty() {
                args.push(format!("--import-to={destination}").into());
            }
            args.push(format!("--import-batch-gib={}", self.import_batch_gib.max(1)).into());
            if self.import_rename {
                args.push("--import-conflict=rename".into());
            }
        }
        if matches!(action, 7 | 8) {
            args.push(format!("--keep-previous={}", self.keep_previous).into());
        }
        if action == 8 {
            args.push("--exclusive-archive-ownership".into());
        }
        if action == 6 && !self.recovery_reprocess_plan.trim().is_empty() {
            args.push("--recovery-reprocess-plan".into());
            args.push(self.recovery_reprocess_plan.trim().into());
        }
        Ok(args)
    }

    pub(super) fn start_action(&mut self, rclone: &str, action: u8) -> Result<(), String> {
        let sync_only = action != 0;
        let control = tempfile::Builder::new()
            .prefix("rpool-mount-control-")
            .tempdir()
            .map_err(|error| {
                trf(
                    "Cannot create mount control directory: {error}",
                    &[("error", &error)],
                )
            })?;
        let args = self.action_args(action, control.path())?;
        self.check_conflict(action)?;
        let spec = self.spec_for(action);
        let diagnostic = self.diagnostic_read_only && self.pool_retention && self.pool_sync;
        let session = &mut self.session;
        session.capacity = None;
        session.pool_status = None;
        session.import_status = None;
        session.runner.start_rpool(
            match action {
                2 => "Check pool capacity",
                3 => "Migrate active archives (retain originals)",
                4 => "Trim clean shard cache",
                5 => "Export recoverable spool",
                6 => "Apply pool changes (preserve original workspace)",
                7 => "Preview obsolete versions",
                8 => "Delete obsolete versions",
                9 => "Import from rclone",
                _ if sync_only => "Sync local workspace",
                _ if diagnostic => "Diagnostic read-only mount",
                _ => "Mount workspace",
            },
            rclone,
            args,
        )?;
        session.started(control, spec, action);
        session.recovering_accounts = false;
        if action == 8 {
            session.retention_previewed = None;
            self.retention_confirmed = false;
        }
        self.session.notice = Some(match action {
            7 => tr("Previewing obsolete versions; nothing is uploaded or deleted. Check the log for the list."),
            8 => tr("Deleting obsolete versions. The operation is resumable; do not remove the retention journal."),
            9 => tr("Importing. The source is only read; imported files upload in batches. Stop anytime and start again to resume."),
            2.. => tr("Maintenance running. See log for capacity, cleanup or recovery results."),
            1 => tr("Synchronizing local workspace. See log for verified archive results."),
            0 => tr("Mount process started; this is not yet proof that the drive is mounted or cloud changes are committed. See log for readiness and sync status."),
        }.into());
        Ok(())
    }
}
