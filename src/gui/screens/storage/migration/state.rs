//! State of the pool change migration wizard: the selected pool, the step,
//! background planning / status work, and the task argv the steps start.
use crate::gui::i18n::{tr, trf};
use crate::migration::drive_model::{DrivePlan, DriveStatus};
use crate::migration::model::{Entry, LostFile, MigrationStatus, Plan};
use crate::migration::plan::PlanOptions;
use crate::models::PoolDefinition;
use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Task names; completion is recognised by them.
pub(crate) const RUN_TASK: &str = "Pool migration run";
/// Task name of `pool migrate abandon` (discard a migration).
pub(crate) const ABANDON_TASK: &str = "Pool migration discard";
/// Task name of `pool migrate adopt` (drive switch).
pub(crate) const ADOPT_TASK: &str = "Pool migration drive adoption";
/// Sample written per remote when measuring speed.
pub(crate) const SPEED_SAMPLE_BYTES: u64 = 8 * 1024 * 1024;
/// Status refresh period while the run step is visible.
pub(crate) const STATUS_REFRESH: Duration = Duration::from_secs(5);

/// Wizard step shown by `migration::card`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Step {
    /// Pick the pool and options, create the plan.
    #[default]
    Plan,
    /// Inspect the freshly created plan before starting it.
    Review,
    /// Run, pause or resume the active migration.
    Run,
    /// Switch the drive to the migrated generation.
    Adopt,
    /// List of files that cannot be recovered (`pool migrate lost`).
    Lost,
    /// After completion: quarantine and delete what the migration left behind.
    Cleanup,
}

/// Result of the planning thread.
#[derive(Debug)]
pub(crate) struct PlanOutcome {
    /// The plan, or the formatted planning error.
    pub(crate) plan: Result<Plan, String>,
    /// The drive part published with the plan, if any.
    pub(crate) drive: Option<DrivePlan>,
    /// Set when speed measurement failed and manual speeds were used.
    pub(crate) speed_note: Option<String>,
}

/// Result of the background status listing (`pool migrate status`).
#[derive(Debug)]
pub(crate) struct StatusOutcome {
    /// Pool the listing was made for; stale results for another pool are dropped.
    pub(crate) pool: String,
    /// Recorded migrations of the pool, or the listing error.
    pub(crate) result: Result<Vec<MigrationStatus>, String>,
    /// Drive part per migration id (migrations with a drive part only).
    pub(crate) drive: std::collections::BTreeMap<String, DriveStatus>,
}

/// `--parallel` of the run step; defaults to the CLI default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunParallel(pub(crate) usize);

impl Default for RunParallel {
    fn default() -> Self {
        Self(crate::migration::execute::DEFAULT_PARALLEL)
    }
}

/// Whether the plan includes the pool's drive; on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IncludeDrive(pub(crate) bool);

impl Default for IncludeDrive {
    fn default() -> Self {
        Self(true)
    }
}

/// Which of our own tasks the console is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Watched {
    /// `pool migrate run` of this migration id.
    Run(String),
    /// `pool migrate abandon` of this migration id.
    Abandon(String),
    /// `pool migrate adopt` of this migration id.
    Adopt(String),
}

/// All state of the migration wizard, held in `GuiState::migration`.
#[derive(Debug, Default)]
pub(crate) struct MigrationForm {
    /// Pool being migrated.
    pub(crate) pool: String,
    /// Take over entries another PC claimed but did not finish.
    pub(crate) take_over: bool,
    /// Archives migrated at once (`--parallel`).
    pub(crate) parallel: RunParallel,
    /// Current wizard step.
    pub(crate) step: Step,
    /// Full probe (hashes) instead of the quick size check when planning.
    pub(crate) probe_full: bool,
    /// Measure download/upload speed before planning (otherwise the manual speeds are used).
    pub(crate) measure_speed: bool,
    /// Plan the pool's drive too (`--no-drive` when false). Default on.
    pub(crate) include_drive: IncludeDrive,
    /// Also move shards of unaffected archives (`--rebalance`).
    pub(crate) rebalance: bool,
    /// Manual speeds, MiB/s; 0 = unknown.
    pub(crate) download_mib_s: f64,
    /// Manual upload speed, MiB/s; 0 = unknown.
    pub(crate) upload_mib_s: f64,
    /// Plan just created (review step).
    pub(crate) plan: Option<Plan>,
    /// Its drive part.
    pub(crate) drive_plan: Option<DrivePlan>,
    /// Drive part of listed migrations, by id.
    pub(crate) drive_statuses: std::collections::BTreeMap<String, DriveStatus>,
    /// Adopt: leave unrecoverable drive files out (`--accept-lost`).
    pub(crate) accept_lost: bool,
    /// Adopt: also switch this PC's drive workspace (`--workspace`).
    pub(crate) switch_workspace: bool,
    /// The workspace to switch; prefilled from the Drive page.
    pub(crate) workspace: String,
    /// Migration shown in the run / lost steps.
    pub(crate) active_id: Option<String>,
    /// Error shown at the top of the wizard card.
    pub(crate) error: Option<String>,
    /// Informational message shown at the top of the wizard card.
    pub(crate) notice: Option<String>,
    /// Migrations of `status_pool` recorded in the cloud.
    pub(crate) statuses: Vec<MigrationStatus>,
    /// Pool the `statuses` belong to; differs from `pool` → reload.
    pub(crate) status_pool: Option<String>,
    /// Error of the last status listing.
    pub(crate) status_error: Option<String>,
    /// When the last status listing started; drives the periodic refresh.
    pub(crate) last_status_at: Option<Instant>,
    /// Whether the "Advanced / manual" section of Account changes is expanded.
    pub(crate) show_advanced: bool,
    /// Our task currently running on the task runner, if any.
    pub(crate) watched: Option<Watched>,
    /// A pause was requested for the running run task.
    pub(crate) pausing: bool,
    /// The "Clean up" step (`pool migrate retire|restore`).
    pub(crate) cleanup: super::cleanup_state::CleanupForm,
    /// Channel of a planning thread in progress.
    planning: Option<Receiver<PlanOutcome>>,
    /// Channel of a status listing in progress.
    status_pending: Option<Receiver<StatusOutcome>>,
    /// Temporary folder holding the run's stop file; writing `stop` pauses the run.
    control: Option<tempfile::TempDir>,
}

impl MigrationForm {
    /// Preselects `pool` (Pools › "Plan migration now") and starts over.
    pub(crate) fn select_pool(&mut self, pool: &str) {
        if self.pool != pool {
            self.pool = pool.to_string();
            self.reset_to_plan();
            self.statuses.clear();
            self.drive_statuses.clear();
            self.status_pool = None;
            self.status_error = None;
        }
    }

    /// Back to the plan step, dropping the plan and the active migration.
    pub(crate) fn reset_to_plan(&mut self) {
        self.step = Step::Plan;
        self.plan = None;
        self.drive_plan = None;
        self.active_id = None;
        self.error = None;
    }

    /// True while a planning thread is running.
    pub(crate) fn planning(&self) -> bool {
        self.planning.is_some()
    }

    /// True while a status listing is running.
    pub(crate) fn status_loading(&self) -> bool {
        self.status_pending.is_some()
    }

    /// Library plan options from the form; non-positive manual speeds become `None`.
    pub(crate) fn plan_options(&self, workers: usize) -> PlanOptions {
        PlanOptions {
            probe_full: self.probe_full,
            download_mib_s: positive(self.download_mib_s),
            upload_mib_s: positive(self.upload_mib_s),
            workers,
            skip_drive: !self.include_drive.0,
            rebalance: self.rebalance,
            ..Default::default()
        }
    }

    /// Starts planning in a background thread (measuring speed first when
    /// asked). `remotes` are the pool's remotes, for the speed sample.
    pub(crate) fn start_plan(&mut self, rclone: &str, remotes: Vec<String>, workers: usize) {
        if self.planning.is_some() || self.pool.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (rclone, pool) = (rclone.to_string(), self.pool.clone());
        let mut options = self.plan_options(workers);
        let measure = self.measure_speed;
        std::thread::spawn(move || {
            let mut speed_note = None;
            if measure {
                match crate::migration::speed::measure(
                    &rclone,
                    &remotes,
                    SPEED_SAMPLE_BYTES,
                    workers,
                ) {
                    Ok(report) => {
                        options.download_mib_s = report.download_mib_s.or(options.download_mib_s);
                        options.upload_mib_s = report.upload_mib_s.or(options.upload_mib_s);
                    }
                    Err(error) => {
                        speed_note = Some(trf(
                            "Speed measurement failed ({error}); the manual speeds were used.",
                            &[("error", &format!("{error:#}"))],
                        ))
                    }
                }
            }
            let (plan, drive) =
                match crate::migration::execute::create_with_drive(&rclone, &pool, &options) {
                    Ok((plan, drive)) => (Ok(plan), drive),
                    Err(error) => (Err(format!("{error:#}")), None),
                };
            let _ = tx.send(PlanOutcome {
                plan,
                drive,
                speed_note,
            });
        });
        self.planning = Some(rx);
        self.error = None;
        self.notice = None;
    }

    /// Starts a background status listing of the selected pool.
    pub(crate) fn start_status(&mut self, rclone: &str) {
        if self.status_pending.is_some() || self.pool.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (rclone, pool) = (rclone.to_string(), self.pool.clone());
        std::thread::spawn(move || {
            let result = crate::migration::status::status(&rclone, &pool, None)
                .map_err(|error| format!("{error:#}"));
            // Drive parts of the migrations still in use (an unreadable one is
            // simply not shown).
            let drive = result
                .iter()
                .flatten()
                .filter(|s| !s.abandoned)
                .filter_map(|s| {
                    crate::migration::drive_status::status(&rclone, &pool, &s.migration_id)
                        .ok()
                        .flatten()
                        .map(|d| (s.migration_id.clone(), d))
                })
                .collect();
            let _ = tx.send(StatusOutcome {
                pool,
                result,
                drive,
            });
        });
        self.status_pending = Some(rx);
        self.last_status_at = Some(Instant::now());
    }

    /// Whether a status listing should start now: first open of the pool,
    /// or periodically while a run step is visible.
    pub(crate) fn status_due(&self, now: Instant) -> bool {
        if self.pool.is_empty() || self.status_pending.is_some() {
            return false;
        }
        if self.status_pool.as_deref() != Some(self.pool.as_str()) {
            return self
                .last_status_at
                .is_none_or(|at| now - at >= STATUS_REFRESH);
        }
        matches!(self.step, Step::Run | Step::Adopt)
            && self
                .last_status_at
                .is_none_or(|at| now - at >= STATUS_REFRESH)
    }

    /// Collects finished background work; true while any is still pending.
    pub(crate) fn poll(&mut self) -> bool {
        if let Some(rx) = &self.planning {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.planning = None;
                    self.apply_plan(outcome);
                }
                Err(TryRecvError::Disconnected) => {
                    self.planning = None;
                    self.error = Some(tr("Planning stopped unexpectedly; try again.").into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.status_pending {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.status_pending = None;
                    self.apply_status(outcome);
                }
                Err(TryRecvError::Disconnected) => {
                    self.status_pending = None;
                    self.status_error = Some(tr("Status listing stopped unexpectedly.").into());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        self.planning.is_some() || self.status_pending.is_some()
    }

    /// Applies a finished plan: opens the review step for it, or reports a
    /// planning error or a plan for a pool no longer selected.
    pub(crate) fn apply_plan(&mut self, outcome: PlanOutcome) {
        self.notice = outcome.speed_note;
        match outcome.plan {
            Ok(plan) if plan.pool == self.pool => {
                self.active_id = Some(plan.migration_id.clone());
                self.drive_plan = outcome
                    .drive
                    .filter(|d| d.migration_id == plan.migration_id);
                self.plan = Some(plan);
                self.step = Step::Review;
                self.error = None;
                // The new migration shows up in the list on the next load.
                self.status_pool = None;
                self.last_status_at = None;
            }
            Ok(plan) => {
                self.error = Some(trf(
                    "A plan for '{planned}' arrived after switching to '{current}'; it was published and is listed under Existing migrations of that pool.",
                    &[("planned", &plan.pool), ("current", &self.pool)],
                ));
            }
            Err(error) => self.error = Some(trf("Planning failed: {error}", &[("error", &error)])),
        }
    }

    /// Applies a finished status listing unless it belongs to another pool.
    pub(crate) fn apply_status(&mut self, outcome: StatusOutcome) {
        if outcome.pool != self.pool {
            return; // stale answer for a previously selected pool
        }
        self.status_pool = Some(outcome.pool);
        match outcome.result {
            Ok(statuses) => {
                self.statuses = statuses;
                self.drive_statuses = outcome.drive;
                self.status_error = None;
            }
            Err(error) => self.status_error = Some(error),
        }
    }

    /// Listed status of the active migration, if loaded.
    pub(crate) fn active_status(&self) -> Option<&MigrationStatus> {
        let id = self.active_id.as_deref()?;
        self.statuses.iter().find(|s| s.migration_id == id)
    }

    /// Drive part of the active migration (listed status).
    pub(crate) fn active_drive_status(&self) -> Option<&DriveStatus> {
        self.drive_statuses.get(self.active_id.as_deref()?)
    }

    /// Lost files of the active migration (archives, then drive files): from
    /// its status when listed, else from the plan under review.
    pub(crate) fn active_lost(&self) -> Vec<LostFile> {
        if let Some(status) = self.active_status() {
            let mut lost = status.lost.clone();
            if let Some(drive) = self.active_drive_status() {
                lost.extend(drive.lost.iter().cloned());
            }
            return lost;
        }
        match (&self.plan, self.active_id.as_deref()) {
            (Some(plan), Some(id)) if plan.migration_id == id => {
                let mut lost = lost_from_plan(plan);
                if let Some(drive) = &self.drive_plan {
                    lost.extend(drive_lost_from_plan(drive));
                }
                lost
            }
            _ => Vec::new(),
        }
    }

    /// Opens `id` in the run step (no task started).
    pub(crate) fn open(&mut self, id: &str) {
        if self.plan.as_ref().is_some_and(|p| p.migration_id != id) {
            self.plan = None;
            self.drive_plan = None;
        }
        self.active_id = Some(id.to_string());
        self.step = Step::Run;
        self.error = None;
        self.last_status_at = None;
    }

    /// Starts `pool migrate run` for `id` on the task runner.
    pub(crate) fn start_run(
        &mut self,
        task: &mut crate::gui::task::TaskRunner,
        rclone: &str,
        id: &str,
    ) -> Result<(), String> {
        let control = tempfile::Builder::new()
            .prefix("rpool-migration-control-")
            .tempdir()
            .map_err(|e| {
                trf(
                    "Cannot create the migration control directory: {error}",
                    &[("error", &e)],
                )
            })?;
        let args = run_args(
            &self.pool,
            id,
            &control.path().join("stop"),
            self.take_over,
            self.parallel.0,
        );
        task.start_rpool(RUN_TASK, rclone, args)?;
        self.control = Some(control);
        self.pausing = false;
        self.watched = Some(Watched::Run(id.to_string()));
        self.open(id);
        Ok(())
    }

    /// Asks the running migration to stop after the current entry.
    pub(crate) fn request_pause(&mut self) -> Result<(), String> {
        let control = self
            .control
            .as_ref()
            .ok_or(tr("No migration is running from this window"))?;
        std::fs::write(control.path().join("stop"), b"stop\n")
            .map_err(|error| trf("Could not request pause: {error}", &[("error", &error)]))?;
        self.pausing = true;
        Ok(())
    }

    /// Starts `pool migrate abandon` (discard) for `id` on the task runner.
    pub(crate) fn start_abandon(
        &mut self,
        task: &mut crate::gui::task::TaskRunner,
        rclone: &str,
        id: &str,
    ) -> Result<(), String> {
        task.start_rpool(ABANDON_TASK, rclone, abandon_args(&self.pool, id))?;
        self.watched = Some(Watched::Abandon(id.to_string()));
        Ok(())
    }

    /// Starts a cleanup step (`args` from [`super::cleanup_state::CleanupForm`]).
    pub(crate) fn start_cleanup(
        &mut self,
        task: &mut crate::gui::task::TaskRunner,
        rclone: &str,
        action: super::cleanup_state::CleanupAction,
        args: Vec<OsString>,
    ) -> Result<(), String> {
        task.start_rpool(action.task_name(), rclone, args)?;
        self.cleanup.confirm_force = None;
        self.cleanup.watched = Some(action);
        Ok(())
    }

    /// Starts `pool migrate adopt` for `id` (the drive switch).
    pub(crate) fn start_adopt(
        &mut self,
        task: &mut crate::gui::task::TaskRunner,
        rclone: &str,
        id: &str,
    ) -> Result<(), String> {
        let control = tempfile::Builder::new()
            .prefix("rpool-migration-control-")
            .tempdir()
            .map_err(|e| {
                trf(
                    "Cannot create the migration control directory: {error}",
                    &[("error", &e)],
                )
            })?;
        let workspace = self.workspace.trim();
        let workspace =
            (self.switch_workspace && !workspace.is_empty()).then(|| Path::new(workspace));
        let args = adopt_args(
            &self.pool,
            id,
            &control.path().join("stop"),
            self.accept_lost,
            workspace,
            self.take_over,
        );
        task.start_rpool(ADOPT_TASK, rclone, args)?;
        self.control = Some(control);
        self.pausing = false;
        self.watched = Some(Watched::Adopt(id.to_string()));
        self.active_id = Some(id.to_string());
        self.step = Step::Adopt;
        self.error = None;
        Ok(())
    }

    /// Call every frame: notices when our task finished.
    pub(crate) fn watch_task(&mut self, task: &crate::gui::task::TaskRunner) {
        if task.is_running() {
            return;
        }
        if let Some(action) = self.cleanup.watched.take() {
            let ok = task
                .last_task()
                .filter(|t| t.name == action.task_name())
                .is_some_and(|t| t.status == crate::gui::task::JobStatus::Completed);
            self.notice = Some(self.cleanup.finish(action, ok));
        }
        let Some(watched) = self.watched.take() else {
            return;
        };
        let name = match &watched {
            Watched::Run(_) => RUN_TASK,
            Watched::Abandon(_) => ABANDON_TASK,
            Watched::Adopt(_) => ADOPT_TASK,
        };
        // Another task may have run since; only our own outcome counts.
        let status = task
            .last_task()
            .filter(|t| t.name == name)
            .map(|t| t.status);
        self.finish_task(watched, status);
    }

    /// Sets the notice/error after our watched task ended and forces a status
    /// reload; `status` is `None` when the outcome is unknown.
    pub(crate) fn finish_task(
        &mut self,
        watched: Watched,
        status: Option<crate::gui::task::JobStatus>,
    ) {
        use crate::gui::task::JobStatus;
        let ok = status == Some(JobStatus::Completed);
        match watched {
            Watched::Run(_) => {
                self.notice = Some(match (ok, self.pausing) {
                    (true, true) => tr("Paused. Resume continues where it stopped, from any PC.").into(),
                    (true, false) => tr("Run finished. Check the counts below; lost files are listed separately.").into(),
                    (false, _) => tr("The run stopped with an error or was cancelled; see the console. Resume retries the remaining entries.").into(),
                });
                self.control = None;
                self.pausing = false;
            }
            Watched::Adopt(_) => {
                if ok {
                    self.notice = Some(tr("The drive now uses the new layout. New drive workspaces open it on every PC; a PC still on the previous layout is asked to switch.").into());
                } else {
                    self.error = Some(tr("Adoption did not finish; see the console. Run it again: it continues where it stopped.").into());
                }
                self.control = None;
                self.pausing = false;
            }
            Watched::Abandon(id) => {
                if ok {
                    self.notice = Some(trf("Migration {id} discarded.", &[("id", &short_id(&id))]));
                    if self.active_id.as_deref() == Some(id.as_str()) {
                        self.reset_to_plan();
                    }
                } else {
                    self.error = Some(tr("Discard failed; see the console.").into());
                }
            }
        }
        self.last_status_at = None;
        self.status_pool = None;
    }
}

/// `Some(value)` for finite positive values, else `None` (unknown speed).
fn positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// `rpool pool migrate run <pool> --id <id> --stop-file <stop> --parallel <n>
/// [--take-over]`.
pub(crate) fn run_args(
    pool: &str,
    id: &str,
    stop: &Path,
    take_over: bool,
    parallel: usize,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "pool".into(),
        "migrate".into(),
        "run".into(),
        "--id".into(),
        id.into(),
        "--stop-file".into(),
        stop.as_os_str().to_owned(),
        "--parallel".into(),
        parallel
            .clamp(1, crate::migration::execute::MAX_PARALLEL)
            .to_string()
            .into(),
    ];
    if take_over {
        args.push("--take-over".into());
    }
    args.extend(["--".into(), pool.into()]);
    args
}

/// `rpool pool migrate abandon <pool> --id <id>`.
pub(crate) fn abandon_args(pool: &str, id: &str) -> Vec<OsString> {
    vec![
        "pool".into(),
        "migrate".into(),
        "abandon".into(),
        "--id".into(),
        id.into(),
        "--".into(),
        pool.into(),
    ]
}

/// `rpool pool migrate adopt <pool> --id <id> --stop-file <stop>
/// [--accept-lost] [--workspace <ws>] [--take-over]`.
pub(crate) fn adopt_args(
    pool: &str,
    id: &str,
    stop: &Path,
    accept_lost: bool,
    workspace: Option<&Path>,
    take_over: bool,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "pool".into(),
        "migrate".into(),
        "adopt".into(),
        "--id".into(),
        id.into(),
        "--stop-file".into(),
        stop.as_os_str().to_owned(),
    ];
    if accept_lost {
        args.push("--accept-lost".into());
    }
    if let Some(workspace) = workspace {
        args.push("--workspace".into());
        args.push(workspace.as_os_str().to_owned());
    }
    if take_over {
        args.push("--take-over".into());
    }
    args.extend(["--".into(), pool.into()]);
    args
}

/// Lost drive files of a drive plan as lost-file rows.
pub(crate) fn drive_lost_from_plan(drive: &DrivePlan) -> Vec<LostFile> {
    drive
        .entries
        .iter()
        .filter(|e| e.action == crate::migration::model::Action::Lost)
        .map(|e| LostFile {
            archive_id: e.source_archive_id.clone(),
            original_name: e.path.clone(),
            size: e.size,
            groups: e.losses.clone(),
            detected: "plan".into(),
        })
        .collect()
}

/// Lost entries of a plan as the lost-file rows.
pub(crate) fn lost_from_plan(plan: &Plan) -> Vec<LostFile> {
    plan.entries
        .iter()
        .filter(|e| e.action == crate::migration::model::Action::Lost)
        .map(|e: &Entry| LostFile {
            archive_id: e.archive_id.clone(),
            original_name: e.original_name.clone(),
            size: e.size,
            groups: e.losses.clone(),
            detected: "plan".into(),
        })
        .collect()
}

/// First 12 characters of a migration id, for display.
pub(crate) fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// Whether a saved pool change affects stored data (remotes, K, M, shard
/// size or native crypt), so a migration should be offered.
pub(crate) fn policy_change_affects_data(old: &PoolDefinition, new: &PoolDefinition) -> bool {
    let set = |p: &PoolDefinition| {
        p.remotes
            .iter()
            .map(|r| r.trim().to_string())
            .collect::<std::collections::BTreeSet<_>>()
    };
    set(old) != set(new)
        || old.data_shards != new.data_shards
        || old.parity_shards != new.parity_shards
        || old.shard_size != new.shard_size
        || old.native_crypt != new.native_crypt
}

/// Translated ETA range text, or "unknown" without speeds.
pub(crate) fn format_eta(range: Option<(f64, f64)>) -> String {
    match range {
        None => tr("unknown (no speed given)").into(),
        some => crate::gui::i18n::duration_text(&crate::migration::speed::format_estimate(some)),
    }
}
