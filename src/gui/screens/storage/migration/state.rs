//! State of the pool change migration wizard: the selected pool, the step,
//! background planning / status work, and the task argv the steps start.
use crate::gui::i18n::{tr, trf};
use crate::migration::model::{Entry, LostFile, MigrationStatus, Plan};
use crate::migration::plan::PlanOptions;
use crate::models::PoolDefinition;
use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Task names; completion is recognised by them.
pub(crate) const RUN_TASK: &str = "Pool migration run";
pub(crate) const ABANDON_TASK: &str = "Pool migration discard";
/// Sample written per remote when measuring speed.
pub(crate) const SPEED_SAMPLE_BYTES: u64 = 8 * 1024 * 1024;
/// Status refresh period while the run step is visible.
pub(crate) const STATUS_REFRESH: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Step {
    #[default]
    Plan,
    Review,
    Run,
    Lost,
}

/// Result of the planning thread.
#[derive(Debug)]
pub(crate) struct PlanOutcome {
    pub(crate) plan: Result<Plan, String>,
    /// Set when speed measurement failed and manual speeds were used.
    pub(crate) speed_note: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StatusOutcome {
    pub(crate) pool: String,
    pub(crate) result: Result<Vec<MigrationStatus>, String>,
}

/// Which of our own tasks the console is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Watched {
    Run(String),
    Abandon(String),
}

#[derive(Debug, Default)]
pub(crate) struct MigrationForm {
    pub(crate) pool: String,
    /// Take over entries another PC claimed but did not finish.
    pub(crate) take_over: bool,
    pub(crate) step: Step,
    pub(crate) probe_full: bool,
    pub(crate) measure_speed: bool,
    /// Manual speeds, MiB/s; 0 = unknown.
    pub(crate) download_mib_s: f64,
    pub(crate) upload_mib_s: f64,
    /// Plan just created (review step).
    pub(crate) plan: Option<Plan>,
    /// Migration shown in the run / lost steps.
    pub(crate) active_id: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) statuses: Vec<MigrationStatus>,
    /// Pool the `statuses` belong to; differs from `pool` → reload.
    pub(crate) status_pool: Option<String>,
    pub(crate) status_error: Option<String>,
    pub(crate) last_status_at: Option<Instant>,
    pub(crate) show_advanced: bool,
    pub(crate) watched: Option<Watched>,
    pub(crate) pausing: bool,
    planning: Option<Receiver<PlanOutcome>>,
    status_pending: Option<Receiver<StatusOutcome>>,
    control: Option<tempfile::TempDir>,
}

impl MigrationForm {
    /// Preselects `pool` (Pools › "Plan migration now") and starts over.
    pub(crate) fn select_pool(&mut self, pool: &str) {
        if self.pool != pool {
            self.pool = pool.to_string();
            self.reset_to_plan();
            self.statuses.clear();
            self.status_pool = None;
            self.status_error = None;
        }
    }

    pub(crate) fn reset_to_plan(&mut self) {
        self.step = Step::Plan;
        self.plan = None;
        self.active_id = None;
        self.error = None;
    }

    pub(crate) fn planning(&self) -> bool {
        self.planning.is_some()
    }

    pub(crate) fn status_loading(&self) -> bool {
        self.status_pending.is_some()
    }

    pub(crate) fn plan_options(&self, workers: usize) -> PlanOptions {
        PlanOptions {
            probe_full: self.probe_full,
            download_mib_s: positive(self.download_mib_s),
            upload_mib_s: positive(self.upload_mib_s),
            workers,
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
            let plan = crate::migration::execute::create(&rclone, &pool, &options)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(PlanOutcome { plan, speed_note });
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
            let _ = tx.send(StatusOutcome { pool, result });
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
        self.step == Step::Run
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

    pub(crate) fn apply_plan(&mut self, outcome: PlanOutcome) {
        self.notice = outcome.speed_note;
        match outcome.plan {
            Ok(plan) if plan.pool == self.pool => {
                self.active_id = Some(plan.migration_id.clone());
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

    pub(crate) fn apply_status(&mut self, outcome: StatusOutcome) {
        if outcome.pool != self.pool {
            return; // stale answer for a previously selected pool
        }
        self.status_pool = Some(outcome.pool);
        match outcome.result {
            Ok(statuses) => {
                self.statuses = statuses;
                self.status_error = None;
            }
            Err(error) => self.status_error = Some(error),
        }
    }

    pub(crate) fn active_status(&self) -> Option<&MigrationStatus> {
        let id = self.active_id.as_deref()?;
        self.statuses.iter().find(|s| s.migration_id == id)
    }

    /// Lost files of the active migration: from its status when listed,
    /// else from the plan under review.
    pub(crate) fn active_lost(&self) -> Vec<LostFile> {
        if let Some(status) = self.active_status() {
            return status.lost.clone();
        }
        match (&self.plan, self.active_id.as_deref()) {
            (Some(plan), Some(id)) if plan.migration_id == id => lost_from_plan(plan),
            _ => Vec::new(),
        }
    }

    /// Opens `id` in the run step (no task started).
    pub(crate) fn open(&mut self, id: &str) {
        if self.plan.as_ref().is_some_and(|p| p.migration_id != id) {
            self.plan = None;
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
        let args = run_args(&self.pool, id, &control.path().join("stop"), self.take_over);
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

    /// Call every frame: notices when our task finished.
    pub(crate) fn watch_task(&mut self, task: &crate::gui::task::TaskRunner) {
        if task.is_running() {
            return;
        }
        let Some(watched) = self.watched.take() else {
            return;
        };
        let name = match &watched {
            Watched::Run(_) => RUN_TASK,
            Watched::Abandon(_) => ABANDON_TASK,
        };
        // Another task may have run since; only our own outcome counts.
        let status = task
            .last_task()
            .filter(|t| t.name == name)
            .map(|t| t.status);
        self.finish_task(watched, status);
    }

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

fn positive(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// `rpool pool migrate run <pool> --id <id> --stop-file <stop> [--take-over]`.
pub(crate) fn run_args(pool: &str, id: &str, stop: &Path, take_over: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "pool".into(),
        "migrate".into(),
        "run".into(),
        "--id".into(),
        id.into(),
        "--stop-file".into(),
        stop.as_os_str().to_owned(),
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

pub(crate) fn format_eta(range: Option<(f64, f64)>) -> String {
    match range {
        None => tr("unknown (no speed given)").into(),
        some => crate::gui::i18n::duration_text(&crate::migration::speed::format_estimate(some)),
    }
}
