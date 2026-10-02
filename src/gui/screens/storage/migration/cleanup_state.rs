//! State of the "Clean up" step: the dry-run report (loaded in the
//! background straight from the library, nothing written), the options,
//! the explicit second confirmation the mass-delete guard asks for, and the
//! argv of `pool migrate retire|restore` the buttons start (CLI parity).
use crate::gui::i18n::{tr, trf};
use crate::migration::retire::model::{RetireOptions, RetireReport, DEFAULT_GRACE_DAYS};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

/// Task name of `pool migrate retire` that moves items into quarantine.
pub(crate) const QUARANTINE_TASK: &str = "Pool migration cleanup quarantine";
/// Task name of the permanent delete after the grace period.
pub(crate) const DELETE_TASK: &str = "Pool migration cleanup delete";
/// Task name of `pool migrate restore` (out of quarantine).
pub(crate) const RESTORE_TASK: &str = "Pool migration cleanup restore";

/// What a cleanup button asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CleanupAction {
    /// Move cleanup candidates into the cleanup quarantine (recorded only, still readable).
    Quarantine,
    /// Permanently delete quarantined items whose grace period ended.
    Delete,
    /// Archive ids; empty = all.
    Restore(Vec<String>),
}

impl CleanupAction {
    /// Task name used to start the action and to recognise its finished task.
    pub(crate) fn task_name(&self) -> &'static str {
        match self {
            Self::Quarantine => QUARANTINE_TASK,
            Self::Delete => DELETE_TASK,
            Self::Restore(_) => RESTORE_TASK,
        }
    }
}

/// Result of a background dry-run cleanup check, sent from the worker thread.
#[derive(Debug)]
pub(crate) struct ReportOutcome {
    /// Migration id the report belongs to.
    pub(crate) id: String,
    /// Report, or the formatted error.
    pub(crate) result: Result<RetireReport, String>,
}

/// GUI state of the migration "Clean up" step, held in `MigrationForm::cleanup`.
#[derive(Debug)]
pub(crate) struct CleanupForm {
    /// Grace period before quarantined items may be deleted, in days (capped at `MAX_GRACE_DAYS`).
    pub(crate) grace_days: u64,
    /// Also clean up items of accounts removed from the pool.
    pub(crate) include_removed: bool,
    /// Re-verify replacements fully instead of the quick check.
    pub(crate) full_verify: bool,
    /// Extra drive workspace folder whose metadata counts as references.
    pub(crate) workspace: String,
    /// Latest dry-run report and the migration it belongs to.
    pub(crate) report: Option<RetireReport>,
    /// Migration id of `report` (also set when the check failed), so the check is not repeated.
    pub(crate) report_id: Option<String>,
    /// Error of the last check, shown in the step.
    pub(crate) error: Option<String>,
    /// The guard refused this action: waiting for the explicit second
    /// confirmation (then `--force` is passed).
    pub(crate) confirm_force: Option<CleanupAction>,
    /// Our running task (to refresh the report when it ends).
    pub(crate) watched: Option<CleanupAction>,
    /// Channel of a dry run in progress; `None` when idle.
    loading: Option<Receiver<ReportOutcome>>,
}

impl Default for CleanupForm {
    fn default() -> Self {
        Self {
            grace_days: DEFAULT_GRACE_DAYS,
            include_removed: false,
            full_verify: false,
            workspace: String::new(),
            report: None,
            report_id: None,
            error: None,
            confirm_force: None,
            watched: None,
            loading: None,
        }
    }
}

impl CleanupForm {
    /// True while a dry-run check is in progress.
    pub(crate) fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Workspaces whose metadata counts as references: running drive
    /// sessions of the pool and the typed folder.
    pub(crate) fn workspaces(&self, running: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = running.to_vec();
        let typed = self.workspace.trim();
        if !typed.is_empty() && !out.iter().any(|w| w.as_os_str() == typed) {
            out.push(PathBuf::from(typed));
        }
        out
    }

    /// Library options for retire from the form (grace days converted to seconds).
    pub(crate) fn options(&self, workspaces: Vec<PathBuf>) -> RetireOptions {
        RetireOptions {
            grace_seconds: self
                .grace_days
                .min(crate::migration::retire::model::MAX_GRACE_DAYS)
                * 86_400,
            include_removed: self.include_removed,
            full_verify: self.full_verify,
            workspaces,
            ..RetireOptions::default()
        }
    }

    /// Starts the dry run of `id` in a background thread.
    pub(crate) fn start_report(
        &mut self,
        rclone: &str,
        pool: &str,
        id: &str,
        workspaces: Vec<PathBuf>,
    ) {
        if self.loading.is_some() || pool.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (rclone, pool, id) = (rclone.to_string(), pool.to_string(), id.to_string());
        let options = self.options(workspaces);
        std::thread::spawn(move || {
            let result = crate::migration::retire::retire(&rclone, &pool, &id, &options)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(ReportOutcome { id, result });
        });
        self.loading = Some(rx);
        self.error = None;
    }

    /// Collects a finished dry run; true while one is pending.
    pub(crate) fn poll(&mut self) -> bool {
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.loading = None;
                    self.apply_report(outcome);
                }
                Err(TryRecvError::Disconnected) => {
                    self.loading = None;
                    self.error =
                        Some(tr("The cleanup check stopped unexpectedly; try again.").into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        self.loading.is_some()
    }

    /// Stores a finished check: the report, or clears it and sets the error.
    pub(crate) fn apply_report(&mut self, outcome: ReportOutcome) {
        match outcome.result {
            Ok(report) => {
                self.report = Some(report);
                self.report_id = Some(outcome.id);
                self.error = None;
            }
            Err(error) => {
                self.report = None;
                self.report_id = Some(outcome.id);
                self.error = Some(trf("Cleanup check failed: {error}", &[("error", &error)]));
            }
        }
    }

    /// The report when it belongs to `id`.
    pub(crate) fn report_for(&self, id: &str) -> Option<&RetireReport> {
        self.report
            .as_ref()
            .filter(|_| self.report_id.as_deref() == Some(id))
    }

    /// Why the guard would refuse `action` (from the latest report).
    pub(crate) fn guard_refusal(&self, action: &CleanupAction) -> Option<&str> {
        let guard = &self.report.as_ref()?.guard;
        match action {
            CleanupAction::Quarantine => guard.quarantine_refusal.as_deref(),
            CleanupAction::Delete => guard.delete_refusal.as_deref(),
            CleanupAction::Restore(_) => None,
        }
    }

    /// First click: either the argv to run, or (guard refused) `None` and
    /// the second confirmation is shown. The confirmation's own button calls
    /// [`CleanupForm::args`] with `force`.
    pub(crate) fn request(
        &mut self,
        action: CleanupAction,
        pool: &str,
        id: &str,
        workspaces: &[PathBuf],
    ) -> Option<Vec<OsString>> {
        if self.guard_refusal(&action).is_some() {
            self.confirm_force = Some(action);
            return None;
        }
        self.confirm_force = None;
        Some(self.args(&action, pool, id, workspaces, false))
    }

    /// `rpool pool migrate retire … --confirm --step …` or `… restore …`.
    pub(crate) fn args(
        &self,
        action: &CleanupAction,
        pool: &str,
        id: &str,
        workspaces: &[PathBuf],
        force: bool,
    ) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["pool".into(), "migrate".into()];
        match action {
            CleanupAction::Restore(items) => {
                args.extend(["restore".into(), "--id".into(), id.into()]);
                if items.is_empty() {
                    args.push("--all".into());
                }
                for item in items {
                    args.extend(["--item".into(), item.into()]);
                }
            }
            CleanupAction::Quarantine | CleanupAction::Delete => {
                let step = if *action == CleanupAction::Quarantine {
                    "quarantine"
                } else {
                    "delete"
                };
                args.extend([
                    "retire".into(),
                    "--id".into(),
                    id.into(),
                    "--confirm".into(),
                    "--step".into(),
                    step.into(),
                    "--grace-days".into(),
                    self.grace_days
                        .min(crate::migration::retire::model::MAX_GRACE_DAYS)
                        .to_string()
                        .into(),
                ]);
                if self.include_removed {
                    args.push("--include-removed-accounts".into());
                }
                if self.full_verify {
                    args.push("--full-verify".into());
                }
                for workspace in workspaces {
                    args.extend(["--workspace".into(), workspace.as_os_str().to_owned()]);
                }
                if force {
                    args.push("--force".into());
                }
            }
        }
        args.extend(["--".into(), pool.into()]);
        args
    }

    /// Our task ended: refresh the report and say how it went.
    pub(crate) fn finish(&mut self, action: CleanupAction, ok: bool) -> String {
        self.report_id = None;
        match (action, ok) {
            (CleanupAction::Quarantine, true) => {
                tr("Moved to the cleanup quarantine. Nothing was deleted; restore is possible until the grace period ends.").into()
            }
            (CleanupAction::Delete, true) => tr("Permanent deletion finished; see the console for what was deleted or kept.").into(),
            (CleanupAction::Restore(_), true) => tr("Restored from the cleanup quarantine.").into(),
            (_, false) => tr("The cleanup step stopped with an error or was refused; see the console. Running it again continues where it stopped.").into(),
        }
    }
}
