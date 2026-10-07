//! Mount screen state, persistence per pool, and the actions it starts.
use super::build_args;
use super::session::{MountSession, SessionSpec};
use crate::gui::i18n::{tr, trf};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

/// Drive page state, held in `GuiState::mount`: the edited per-pool mount
/// settings plus the running sessions; builds and starts `rpool mount` actions.
pub(crate) struct MountForm {
    /// The selected pool's session (idle when that pool does not run).
    pub(super) session: MountSession,
    /// Sessions of other pools that ran when another pool was selected.
    pub(super) background: BTreeMap<String, MountSession>,
    /// Selected pool; switching saves the old pool's profile (`select_pool`).
    pub(super) pool: String,
    /// Persistent local workspace folder (`--workspace`); must be absolute.
    pub(super) workspace: String,
    /// Windows drive letter or empty Unix folder (`--mountpoint`); default `R:` on Windows.
    pub(super) mountpoint: String,
    /// Display name in pool-sync conflicts (`--pool-worker`); generated if empty.
    pub(super) pc_name: String,
    /// Archive manifests to add to the drive at the next start (`--manifest`).
    pub(super) manifests: Vec<String>,
    /// Text box of the manifest to add on the Import tab.
    pub(super) manifest_input: String,
    /// Background sync interval in seconds (`--interval-seconds`).
    pub(super) interval_seconds: u64,
    /// Shard transfers of this PC (`--workers`), copied from Settings before
    /// each start; 0 = the pool's saved value.
    pub(super) workers: u64,
    /// Clean shard cache budget, GiB (`--cache-gib`).
    pub(super) cache_gib: u64,
    /// OS / native mount cache target, GiB (`--vfs-cache-gib`).
    pub(super) vfs_cache_gib: u64,
    /// Disk space the OS cache tries to leave free, GiB (`--cache-min-free-gib`).
    pub(super) cache_min_free_gib: u64,
    /// Pending-write (spool) limit, GiB (`--spool-gib`).
    pub(super) spool_gib: u64,
    /// Original workspace to recover from (`--account-recovery-from`).
    pub(super) recovery_source: String,
    /// Plain rclone `remote:path` to import into the drive.
    pub(super) import_source: String,
    /// Drive folder the import lands in; empty for the root.
    pub(super) import_destination: String,
    /// Upload after this many GiB were copied during an rclone import (min 1).
    pub(super) import_batch_gib: u64,
    /// Import name clashes as renamed copies (`--import-conflict=rename`) instead of skipping.
    pub(super) import_rename: bool,
    /// Selected Drive page tab.
    pub(crate) tab: crate::gui::state::DriveTab,
    /// Remotes to skip during recovery, one per line (`--recovery-skip-remote`).
    pub(super) recovery_skip_remotes: String,
    /// Completed reprocess `plan.json` to reuse (`--recovery-reprocess-plan`).
    pub(super) recovery_reprocess_plan: String,
    /// Filesystem frontend chosen on the Options tab.
    pub(super) frontend: crate::cli::Frontend,
    /// Mount read-only (`--native-read-only`, native frontends only).
    pub(super) native_read_only: bool,
    /// Drive › Maintenance › Workspace backups.
    pub(super) backups: super::backups_card::BackupsView,
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
            pc_name: String::new(),
            manifests: Vec::new(),
            manifest_input: String::new(),
            interval_seconds: 30,
            workers: 0,
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
            frontend: Default::default(),
            native_read_only: false,
            backups: Default::default(),
        }
    }
}

impl MountForm {
    /// This PC's drive workspace of `pool`, when the Drive page has one set
    /// (pool migration offers to switch it after adopting the drive).
    pub(crate) fn workspace_of(&self, pool: &str) -> Option<String> {
        let workspace = self.workspace.trim();
        (self.pool == pool && !workspace.is_empty()).then(|| workspace.to_string())
    }
    /// Selects a completed reprocess plan for recovery; called from `reprocess`.
    pub(crate) fn use_reprocess_plan(&mut self, path: &Path) {
        self.recovery_reprocess_plan = path.display().to_string();
        self.session.notice = Some(tr("Reprocess plan selected. Open Recover after account removal, choose the original workspace and a new destination pool/workspace. Only validated completed replacements will be reused.").into());
    }
    /// Default form with the cache budgets from the GUI settings; used at startup.
    pub(crate) fn from_settings(settings: &crate::gui::settings::GuiSettings) -> Self {
        let cache = &settings.mount_cache;
        Self {
            cache_gib: cache.shard_gib,
            vfs_cache_gib: cache.native_gib,
            cache_min_free_gib: cache.min_free_gib,
            spool_gib: cache.spool_gib,
            ..Self::default()
        }
    }

    /// Current cache budgets as the saved settings type.
    pub(super) fn cache_settings(&self) -> crate::gui::settings::MountCacheSettings {
        crate::gui::settings::MountCacheSettings {
            shard_gib: self.cache_gib,
            native_gib: self.vfs_cache_gib,
            min_free_gib: self.cache_min_free_gib,
            spool_gib: self.spool_gib,
        }
    }

    /// Saves the form into the GUI settings (per-pool profile, or the global
    /// cache budgets when no pool is selected) and writes them to disk.
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

    /// The per-pool profile saved in `GuiSettings::mount_profiles`.
    pub(super) fn profile(&self) -> crate::gui::settings::MountProfile {
        crate::gui::settings::MountProfile {
            workspace: self.workspace.clone(),
            mountpoint: self.mountpoint.clone(),
            pc_name: self.pc_name.clone(),
            manifests: self.manifests.clone(),
            interval_seconds: self.interval_seconds,
            cache: self.cache_settings(),
            frontend: self.frontend,
            native_read_only: self.native_read_only,
        }
    }

    /// Switches the Drive page to `pool`: saves the old profile, keeps a
    /// running session in `background`, and loads the new pool's profile and
    /// session (avoiding a taken drive letter for a first-time pool).
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
        self.pc_name = profile.pc_name;
        self.manifests = profile.manifests;
        self.interval_seconds = profile.interval_seconds;
        self.frontend = profile.frontend;
        self.native_read_only = profile.native_read_only;
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

    /// Whether this mount will use a native frontend on this build.
    pub(super) fn native_selected(&self) -> bool {
        use crate::cli::Frontend;
        Frontend::native_here().is_some()
            && matches!(
                self.frontend,
                Frontend::Auto | Frontend::Fuse | Frontend::Winfsp
            )
    }
    /// `--frontend` for a mount (never for sync or maintenance actions). Always
    /// explicit, so the CLI's `auto` default never picks a frontend the form
    /// did not offer.
    pub(super) fn frontend_args(&self, sync_only: bool) -> Vec<OsString> {
        if sync_only {
            return vec![];
        }
        let mut args = vec![OsString::from(format!(
            "--frontend={}",
            self.frontend.cli_value()
        ))];
        if self.native_read_only && self.native_selected() {
            args.push("--native-read-only".into());
        }
        args
    }

    /// Appends cache budgets and `--workers` (when set) to `args`.
    pub(super) fn append_cache_args(&self, args: &mut Vec<OsString>) {
        args.push(format!("--vfs-cache-gib={}", self.vfs_cache_gib).into());
        args.push(format!("--cache-min-free-gib={}", self.cache_min_free_gib).into());
        args.push(format!("--spool-gib={}", self.spool_gib).into());
        args.push(format!("--cache-gib={}", self.cache_gib).into());
        if self.workers > 0 {
            args.push(format!("--workers={}", self.workers.clamp(1, 256)).into());
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

    /// Requests a graceful unmount of the selected pool's session.
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
        }
    }

    /// Errors with the conflict message when `action` would clash with a running session.
    fn check_conflict(&self, action: u8) -> Result<(), String> {
        match self.conflict(action) {
            Some(conflict) => Err(conflict.message()),
            None => Ok(()),
        }
    }

    /// Starts a mount (`sync_only` = false) or a workspace sync.
    pub(super) fn start(&mut self, rclone: &str, sync_only: bool) -> Result<(), String> {
        self.start_action(rclone, if sync_only { 1 } else { 0 })
    }

    /// Validated argv of account recovery into the selected (new) pool from
    /// `recovery_source`.
    pub(super) fn recovery_args(&self, stop: &Path) -> Result<Vec<OsString>, String> {
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
            "--stop-file".into(),
            stop.as_os_str().to_owned(),
        ];
        self.append_cache_args(&mut args);
        self.append_pc_name(&mut args);
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

    /// Starts account recovery in the selected session with a fresh control
    /// directory; does not mount a drive.
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
        session.layout_status = None;
        session.notice = Some(tr("Copying the locally known file view into the new destination. This does not mount a drive or remove source data. Review unresolved files in the report before treating recovery as complete.").into());
        Ok(())
    }

    /// `--pool-worker` when a PC name is set.
    fn append_pc_name(&self, args: &mut Vec<OsString>) {
        if !self.pc_name.trim().is_empty() {
            args.push(format!("--pool-worker={}", self.pc_name.trim()).into());
        }
    }

    /// Validated `rpool mount` arguments for `action`: 0 mount, 1 sync, 2
    /// capacity, 4 trim cache, 5 export spool, 6 apply pool changes, 9 import
    /// from rclone.
    pub(super) fn action_args(&self, action: u8, control: &Path) -> Result<Vec<OsString>, String> {
        let sync_only = action != 0;
        if action == 6 && !self.manifests.is_empty() {
            return Err(tr(
                "Apply pool changes needs no listed archive imports; remove them first.",
            )
            .into());
        }
        if action == 9 && !self.import_source.trim().contains(':') {
            return Err(tr(
                "Enter the rclone source as remote:path (for example old-crypt:photos).",
            )
            .into());
        }
        if self.pool.trim().is_empty() || self.workspace.trim().is_empty() {
            return Err(tr("Select an upload pool and a persistent local workspace.").into());
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
            // Listed archives are applied at the next mount; an rclone import
            // is its own offline action and must not carry them.
            if action == 9 { &[] } else { &self.manifests },
            self.interval_seconds,
            &control.join("stop"),
            sync_only,
        );
        self.append_cache_args(&mut args);
        self.append_pc_name(&mut args);
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
                    9 => "--import-from",
                    _ => return Err(format!("unknown mount action {action}")),
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
        if action == 6 && !self.recovery_reprocess_plan.trim().is_empty() {
            args.push("--recovery-reprocess-plan".into());
            args.push(self.recovery_reprocess_plan.trim().into());
        }
        Ok(args)
    }

    /// Starts mount `action` (see `action_args`) in the selected session with a
    /// fresh control directory, after the conflict check; sets the notice.
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
        let session = &mut self.session;
        session.capacity = None;
        session.pool_status = None;
        session.layout_status = None;
        session.import_status = None;
        session.runner.start_rpool(
            match action {
                2 => "Check pool capacity",
                4 => "Trim clean shard cache",
                5 => "Export recoverable spool",
                6 => "Apply pool changes (preserve original workspace)",
                9 => "Import from rclone",
                _ if sync_only => "Sync local workspace",
                _ => "Mount workspace",
            },
            rclone,
            args,
        )?;
        session.started(control, spec, action);
        session.recovering_accounts = false;
        self.session.notice = Some(match action {
            9 => tr("Importing. The source is only read; imported files upload in batches. Stop anytime and start again to resume."),
            2.. => tr("Maintenance running. See log for capacity, cleanup or recovery results."),
            1 => tr("Synchronizing local workspace. See log for verified archive results."),
            0 => tr("Mount process started; this is not yet proof that the drive is mounted or cloud changes are committed. See log for readiness and sync status."),
        }.into());
        Ok(())
    }
}
