//! The runtime of one pool's `rpool mount …` process: its runner, control
//! directory, status files, log and notices. A [`super::MountForm`] edits the
//! selected pool's settings and owns one session per pool that runs.
use crate::gui::i18n::{tr, trf};
use crate::gui::task::{JobStatus, TaskRunner};

/// What a session was started with. The form may show another pool while
/// the session keeps running, so conflicts and the session list use this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSpec {
    /// Pool the session runs.
    pub(crate) pool: String,
    /// Workspace of the session.
    pub(crate) workspace: String,
    /// Empty unless the session mounts a drive.
    pub(crate) mountpoint: String,
    /// The frontend a drive mount uses; `None` for sync and maintenance.
    pub(crate) frontend: Option<crate::cli::Frontend>,
    /// Other workspaces the process reads (the recovery source).
    pub(crate) reads: Vec<String>,
}

/// One pool's running (or last finished) `rpool mount` process and the status
/// it reports. The selected pool's lives in `MountForm::session`, others in
/// `MountForm::background`.
pub(crate) struct MountSession {
    /// What the process was started with; `None` before the first start.
    pub(super) spec: Option<SessionSpec>,
    /// Runs the child process and collects its log.
    pub(super) runner: TaskRunner,
    /// Temporary control directory (stop file and status JSON files); dropped when the process ends.
    pub(super) control: Option<tempfile::TempDir>,
    /// A graceful unmount was requested.
    pub(super) stopping: bool,
    /// Message shown on the Drive page (start, finish or error).
    pub(super) notice: Option<String>,
    /// Last capacity snapshot (`capacity.json`).
    pub(super) capacity: Option<crate::mount::capacity::CapacityStatus>,
    /// When the status files were last read; they are re-read every second.
    pub(super) capacity_read: std::time::Instant,
    /// Last pool-sync status (`pool-sync-status.json`), source of the conflicts list.
    pub(super) pool_status: Option<crate::mount::pool_sync::Status>,
    /// A pool layout change the running mount deferred (pending old-layout uploads).
    pub(super) layout_status: Option<crate::mount::layout_refresh::Deferral>,
    /// Unsaved WebDAV writes the last mount recovered from rclone's cache.
    pub(super) cache_recovery: Vec<crate::mount::cache_recovery::RecoveryReport>,
    /// Progress of an rclone import (`import-status.json`).
    pub(super) import_status: Option<crate::mount::rclone_import::Status>,
    /// The process is an account recovery, not a mount action.
    pub(super) recovering_accounts: bool,
    /// Mount action the process was started with (0 mount, 1 sync, 2+ maintenance; see `MountForm::action_args`).
    pub(super) last_action: u8,
}

impl Default for MountSession {
    fn default() -> Self {
        Self {
            spec: None,
            runner: TaskRunner::default(),
            control: None,
            stopping: false,
            notice: None,
            capacity: None,
            capacity_read: std::time::Instant::now(),
            pool_status: None,
            layout_status: None,
            cache_recovery: Vec::new(),
            import_status: None,
            recovering_accounts: false,
            last_action: 0,
        }
    }
}

impl MountSession {
    /// Whether the session's process is running.
    pub(crate) fn is_running(&self) -> bool {
        self.runner.is_running()
    }

    /// A running drive mount (not sync, maintenance or recovery).
    pub(crate) fn is_mounted(&self) -> bool {
        self.is_running()
            && self.last_action == 0
            && !self.recovering_accounts
            && self
                .spec
                .as_ref()
                .is_some_and(|spec| spec.frontend.is_some())
    }

    /// Reads the status files and finishes the session when its process
    /// ended. `workspace` is where the cache-recovery report is read from.
    pub(super) fn poll(&mut self, workspace: &str) {
        let terminal = self.runner.poll();
        if terminal.is_some() || self.capacity_read.elapsed() >= std::time::Duration::from_secs(1) {
            if let Some(control) = &self.control {
                self.pool_status = std::fs::read(control.path().join("pool-sync-status.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                self.layout_status = std::fs::read(
                    control
                        .path()
                        .join(crate::mount::layout_refresh::STATUS_FILE),
                )
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                self.import_status = std::fs::read(control.path().join("import-status.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                self.capacity = std::fs::read(control.path().join("capacity.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            }
            self.cache_recovery = super::cache_recovery::load(workspace);
            self.capacity_read = std::time::Instant::now();
        }
        if let Some(status) = terminal {
            self.finish(status);
        }
    }

    /// Sets the finish notice for the ended process and drops the control directory.
    fn finish(&mut self, status: JobStatus) {
        self.notice = Some(if self.recovering_accounts {
            match status {
                JobStatus::Completed => tr("Recovery copy completed for the locally known source view. Inspect the recovery report, then mount this destination normally for reading and new writes. Original workspace retained.").into(),
                _ => tr("Recovery stopped or is incomplete. See log and destination recovery report; original data is retained. Resume with the same source/destination, or mount the destination to use already recovered files and save new files.").into(),
            }
        } else {
            match status {
                JobStatus::Completed => tr("Mount/sync process finished. Check the log for writeback results; local workspace and VFS cache are retained.").into(),
                JobStatus::Cancelled => tr("Process force-stopped. Local files and VFS cache are retained; restart the same workspace to recover pending changes.").into(),
                _ => tr("Mount/sync failed. Check the log; local files and cache are retained. Windows mounts require WinFsp.").into(),
            }
        });
        self.stopping = false;
        self.control = None;
    }

    /// Asks the process to unmount gracefully through its stop file.
    pub(super) fn request_stop(&mut self) -> Result<(), String> {
        let control = self
            .control
            .as_ref()
            .ok_or(tr("No mount control directory is available"))?;
        std::fs::write(control.path().join("stop"), b"stop\n")
            .map_err(|error| trf("Could not request unmount: {error}", &[("error", &error)]))?;
        self.stopping = true;
        Ok(())
    }

    /// Records a started process; the caller already started `runner`.
    pub(super) fn started(&mut self, control: tempfile::TempDir, spec: SessionSpec, action: u8) {
        self.control = Some(control);
        self.spec = Some(spec);
        self.stopping = false;
        self.last_action = action;
    }
}

impl Drop for MountSession {
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

#[cfg(test)]
impl MountSession {
    /// A session whose runner reports running without a child process.
    pub(crate) fn fake_running(spec: SessionSpec, control: Option<tempfile::TempDir>) -> Self {
        let mut session = Self::default();
        session.runner.fake_running("Mount workspace");
        session.spec = Some(spec);
        session.control = control;
        session
    }
}
