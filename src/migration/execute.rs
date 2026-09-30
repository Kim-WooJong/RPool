//! Orchestrator (work package C): create a migration (plan + publish), run it
//! with resume, abandon it.
//!
//! Resume is driven only by the cloud journal: records are folded per entry
//! (see [`progress`]) and every decision is idempotent. The run never deletes
//! anything; old archives stay readable, replacements always get a new id.
use super::journal::Journal;
use super::model::{self, Action, Entry, GroupLoss, Plan, Record, RecordState};
use super::plan::PlanOptions;
use crate::prelude::*;

/// Advisory lease: another PC's claim younger than this is left alone.
pub(crate) const CLAIM_LEASE_SECONDS: u64 = 2 * 60 * 60;

/// Plans `pool` and publishes the plan to the cloud journal.
pub(crate) fn create(rclone: &str, pool: &str, options: &PlanOptions) -> Result<Plan> {
    let plan = super::plan::plan(rclone, pool, options)?;
    Journal::open(rclone, pool, &plan.migration_id)?.publish_plan(&plan)?;
    Ok(plan)
}

/// Archives migrated at once when `--parallel` is not given.
pub(crate) const DEFAULT_PARALLEL: usize = 4;
/// Upper bound for `--parallel` (each archive also runs the pool's own
/// relocation workers, so N archives use up to N x workers rclone calls).
pub(crate) const MAX_PARALLEL: usize = 16;

#[derive(Debug, Clone, Default)]
pub(crate) struct RunOptions {
    pub stop_file: Option<PathBuf>,
    /// Treat other PCs' unexpired claims as abandoned (after a PC crashed).
    pub take_over: bool,
    /// Archives processed concurrently (None: [`DEFAULT_PARALLEL`]).
    pub parallel: Option<usize>,
}

/// Worker count actually used: the request (or the default) clamped to
/// `1..=MAX_PARALLEL` and to the number of archives to move.
pub(crate) fn effective_parallel(requested: Option<usize>, work: usize) -> usize {
    requested
        .unwrap_or(DEFAULT_PARALLEL)
        .clamp(1, MAX_PARALLEL)
        .min(work.max(1))
}

/// Error a relocation/re-encode returns when some group has fewer than K
/// readable shards. `run` records the entry as `Lost` with these losses; any
/// other error is treated as a provider problem (`Unknown`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unrecoverable(pub Vec<GroupLoss>);

impl std::fmt::Display for Unrecoverable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LostGroup: {} group(s) have fewer than K readable shards",
            self.0.len()
        )
    }
}
impl std::error::Error for Unrecoverable {}

/// Runs (or resumes) `migration_id` of `pool` until every movable entry is
/// switched, the stop file appears, or an error stops it. Never deletes.
pub(crate) fn run(
    rclone: &str,
    pool: &str,
    migration_id: &str,
    options: &RunOptions,
) -> Result<()> {
    let journal = Journal::open(rclone, pool, migration_id)?;
    let plan = journal
        .load_plan()?
        .ok_or_else(|| anyhow!("migration not found in the cloud: {migration_id}"))?;
    if plan.migration_id != migration_id || plan.pool != pool {
        bail!("journal plan does not belong to {pool}/{migration_id}");
    }
    let records = journal.records()?;
    let work_root = crate::config::app_config_dir()?
        .join("migrations")
        .join(migration_id);
    fs::create_dir_all(&work_root)?;
    let effects = LiveEffects {
        rclone,
        journal: &journal,
        plan: &plan,
        work_root,
        stop_file: options.stop_file.clone(),
        take_over: options.take_over,
        switch_lock: Mutex::new(()),
    };
    println!(
        "migration_parallel={}",
        effective_parallel(options.parallel, order(&plan.entries).len())
    );
    let pc = pc_id();
    let mut summary = run_core(&plan, &records, &pc, &effects, options.parallel)?;
    if !summary.stopped && !summary.unknown.is_empty() {
        // A provider error on one attempt is often transient: give the
        // archives that ended unknown in this run one fresh attempt.
        let retry = unknown_entries(&plan, &journal.records()?);
        if !retry.is_empty() {
            println!("migration_retry_unknown={}", retry.len());
            std::thread::sleep(RETRY_UNKNOWN_DELAY);
            let names: Vec<String> = retry.iter().map(|e| e.original_name.clone()).collect();
            let retry_plan = Plan {
                entries: retry,
                ..plan.clone()
            };
            let second = run_core(
                &retry_plan,
                &journal.records()?,
                &pc,
                &effects,
                options.parallel,
            )?;
            summary.unknown.retain(|u| !names.contains(u));
            summary.unknown.extend(second.unknown);
            summary.switched_now += second.switched_now;
            summary.lost += second.lost;
            summary.claimed_elsewhere += second.claimed_elsewhere;
            summary.stopped = second.stopped;
        }
    }
    summary.finish()
}

/// Movable entries whose latest attempt ended unknown, in dispatch order.
pub(crate) fn unknown_entries(plan: &Plan, records: &[Record]) -> Vec<Entry> {
    let state = progress(records);
    order(&plan.entries)
        .into_iter()
        .filter(|e| matches!(state.get(&e.archive_id), Some(Progress::Unknown(_))))
        .cloned()
        .collect()
}

/// Pause before the one automatic retry of archives that ended unknown.
const RETRY_UNKNOWN_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// Marks a migration abandoned (other PCs stop offering it).
pub(crate) fn abandon(rclone: &str, pool: &str, migration_id: &str) -> Result<()> {
    let journal = Journal::open(rclone, pool, migration_id)?;
    if journal.load_plan()?.is_none() {
        bail!("migration not found in the cloud: {migration_id}");
    }
    journal.append(&Record {
        entry: String::new(),
        state: RecordState::Abandoned,
        attempt_id: random_hex(12)?,
        pc_id: pc_id(),
        ts_unix: crate::utils::now_unix(),
        new_archive_id: None,
        new_manifest: None,
        losses: vec![],
        detail: Some("abandoned by user; nothing was deleted".into()),
    })?;
    println!("migration_abandoned={migration_id}");
    Ok(())
}

/// Name of this PC as recorded in the journal.
pub(crate) fn pc_id() -> String {
    for key in ["RPOOL_PC_ID", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-pc".into())
}

pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("random identifier: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------------------
// Journal folding for resume decisions.

/// Effective progress of one entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Progress {
    /// Latest claim (may be stale or from another PC).
    Claimed(Record),
    Verified(Record),
    Switched(Record),
    Lost(Record),
    /// Last attempt ended with a provider error (no later claim).
    Unknown(Record),
}

/// Folds all records per entry. Completion states (`Switched` > `Verified`)
/// come from [`model::fold`] over the progress states only, so a transient
/// `Unknown`/`Orphan` record never hides a later success. `Unknown` counts
/// only when it is at least as recent as the latest claim.
pub(crate) fn progress(records: &[Record]) -> BTreeMap<String, Progress> {
    let progress_records: Vec<Record> = records
        .iter()
        .filter(|r| {
            !r.entry.is_empty()
                && matches!(
                    r.state,
                    RecordState::Claimed
                        | RecordState::Verified
                        | RecordState::Switched
                        | RecordState::Lost
                )
        })
        .cloned()
        .collect();
    let folded = model::fold(&progress_records);
    // Latest claim per entry (fold keeps the earliest among equal states).
    let mut latest_claim: BTreeMap<&str, &Record> = BTreeMap::new();
    let mut latest_unknown: BTreeMap<&str, &Record> = BTreeMap::new();
    for r in records {
        let map = match r.state {
            RecordState::Claimed => &mut latest_claim,
            RecordState::Unknown => &mut latest_unknown,
            _ => continue,
        };
        let slot = map.entry(r.entry.as_str()).or_insert(r);
        if (r.ts_unix, &r.attempt_id) > (slot.ts_unix, &slot.attempt_id) {
            *slot = r;
        }
    }
    let mut out = BTreeMap::new();
    for (entry, record) in folded {
        let state = match record.state {
            RecordState::Switched => Progress::Switched(record),
            RecordState::Verified => Progress::Verified(record),
            RecordState::Lost => Progress::Lost(record),
            _ => {
                let claim = latest_claim
                    .get(entry.as_str())
                    .map(|r| (*r).clone())
                    .unwrap_or(record);
                match latest_unknown.get(entry.as_str()) {
                    Some(u) if u.ts_unix >= claim.ts_unix => Progress::Unknown((*u).clone()),
                    _ => Progress::Claimed(claim),
                }
            }
        };
        out.insert(entry, state);
    }
    for (entry, unknown) in latest_unknown {
        if !entry.is_empty() && !out.contains_key(entry) {
            out.insert(entry.to_string(), Progress::Unknown(unknown.clone()));
        }
    }
    out
}

/// Migration-level abandonment.
pub(crate) fn is_abandoned(records: &[Record]) -> bool {
    records.iter().any(|r| r.state == RecordState::Abandoned)
}

/// Lowest redundancy margin (available - K over lossy groups); None when the
/// entry has no recorded losses.
fn margin(entry: &Entry) -> Option<i64> {
    entry
        .losses
        .iter()
        .map(|g| g.available as i64 - g.required_k as i64)
        .min()
}

/// Work order: entries with losses first (lowest margin first), then by size.
pub(crate) fn order(entries: &[Entry]) -> Vec<&Entry> {
    let mut out: Vec<&Entry> = entries
        .iter()
        .filter(|e| matches!(e.action, Action::Relocate | Action::Reencode))
        .collect();
    out.sort_by(|a, b| {
        let key = |e: &Entry| (margin(e).is_none(), margin(e).unwrap_or(i64::MAX), e.size);
        key(a)
            .cmp(&key(b))
            .then_with(|| a.archive_id.cmp(&b.archive_id))
    });
    out
}

// ---------------------------------------------------------------------------
// Side-effect boundary, faked in tests.

/// A verified replacement.
#[derive(Debug, Clone)]
pub(crate) struct Replacement {
    pub new_archive_id: String,
    /// `remote:path/manifest.json` of one replica.
    pub new_manifest: String,
}

/// Shared by all run workers, hence `&self` and `Sync`: implementations
/// serialize whatever must not run concurrently (see [`LiveEffects`]).
pub(crate) trait Effects: Sync {
    /// Must be safe to call concurrently for records of different entries.
    fn append(&self, record: &Record) -> Result<()>;
    /// Fingerprint of the entry's current source manifest (Err: provider error).
    fn source_fingerprint(&self, entry: &Entry) -> Result<String>;
    /// Builds and fully verifies the replacement (relocate or re-encode).
    /// An [`Unrecoverable`] error means lost; any other error is unknown.
    fn build(&self, entry: &Entry, new_archive_id: &str) -> Result<Replacement>;
    /// Quick check that a recorded replacement manifest loads and validates.
    fn reverify(&self, entry: &Entry, new_archive_id: &str, new_manifest: &str) -> Result<()>;
    /// Makes the replacement visible (inventory + replacement note).
    fn switch(&self, entry: &Entry, replacement: &Replacement) -> Result<()>;
    fn stop_requested(&self) -> bool;
    /// Other PCs' unexpired claims may be taken over (the user asked for it).
    fn take_over(&self) -> bool {
        false
    }
    fn now(&self) -> u64 {
        crate::utils::now_unix()
    }
    fn new_id(&self) -> Result<String> {
        random_hex(12)
    }
    /// One whole output line (`println!` locks stdout, so concurrent lines
    /// never interleave mid-line).
    fn say(&self, line: &str) {
        println!("{line}");
    }
}

/// Outcome of one run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RunSummary {
    pub total: usize,
    pub switched_now: usize,
    pub already_switched: usize,
    pub lost: usize,
    pub unknown: Vec<String>,
    /// Entries left alone because another PC holds a fresh claim.
    pub claimed_elsewhere: usize,
    pub stopped: bool,
}

impl RunSummary {
    pub(crate) fn finish(self) -> Result<()> {
        println!(
            "migration_summary total={} switched_now={} already_switched={} lost={} unknown={} claimed_elsewhere={} stopped={}",
            self.total,
            self.switched_now,
            self.already_switched,
            self.lost,
            self.unknown.len(),
            self.claimed_elsewhere,
            self.stopped
        );
        if self.stopped {
            bail!("migration stopped by the stop file; run again to resume");
        }
        if !self.unknown.is_empty() {
            bail!(
                "{} entr(y/ies) ended unknown (provider errors or changed sources); run again to retry: {}",
                self.unknown.len(),
                self.unknown.join(", ")
            );
        }
        if self.claimed_elsewhere > 0 {
            bail!(
                "{} entr(y/ies) are claimed by another PC; run again later, or use --take-over if that PC stopped",
                self.claimed_elsewhere
            );
        }
        Ok(())
    }
}

fn record(
    effects: &dyn Effects,
    entry: &str,
    state: RecordState,
    attempt_id: &str,
    pc: &str,
) -> Record {
    Record {
        entry: entry.to_string(),
        state,
        attempt_id: attempt_id.to_string(),
        pc_id: pc.to_string(),
        ts_unix: effects.now(),
        new_archive_id: None,
        new_manifest: None,
        losses: vec![],
        detail: None,
    }
}

/// What happened to one work entry during this run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    SwitchedNow,
    AlreadySwitched,
    Lost,
    /// Original name, for the summary.
    Unknown(String),
    ClaimedElsewhere,
}

/// The resume/run state machine over a frozen plan and the journal records.
///
/// Up to `parallel` archives (see [`effective_parallel`]) are processed at
/// once; each archive's own steps stay strictly ordered, and archives are
/// dispatched in [`order`]. With one worker this is the plain sequential
/// loop. An error that is not about one archive (e.g. the journal refusing
/// appends) stops dispatching, lets in-flight archives finish, and is
/// returned; the first such error wins.
pub(crate) fn run_core(
    plan: &Plan,
    records: &[Record],
    pc: &str,
    effects: &dyn Effects,
    parallel: Option<usize>,
) -> Result<RunSummary> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::PoisonError;

    if is_abandoned(records) {
        bail!(
            "migration {} was abandoned; create a new plan",
            plan.migration_id
        );
    }
    let state = progress(records);
    let mut summary = RunSummary::default();

    // Plan-time losses are recorded once so every PC's status shows them.
    for entry in plan.entries.iter().filter(|e| e.action == Action::Lost) {
        if !matches!(state.get(&entry.archive_id), Some(Progress::Lost(_))) {
            let attempt = effects.new_id()?;
            let mut r = record(effects, &entry.archive_id, RecordState::Lost, &attempt, pc);
            r.losses = entry.losses.clone();
            r.detail = Some("unrecoverable at plan time".into());
            effects.append(&r)?;
        }
        summary.lost += 1;
    }
    for entry in plan.entries.iter().filter(|e| e.action == Action::Unknown) {
        summary.unknown.push(format!(
            "{} (not checked at plan time: {}; create a new plan)",
            entry.original_name,
            entry.detail.as_deref().unwrap_or("provider error")
        ));
    }

    let work = order(&plan.entries);
    summary.total = work.len();
    let total = work.len();
    let workers = effective_parallel(parallel, total);

    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let halted = AtomicBool::new(false);
    let outcomes: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::with_capacity(total));
    let fatal: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let worker = || loop {
        if halted.load(SeqCst) || stopped.load(SeqCst) {
            break;
        }
        let index = next.fetch_add(1, SeqCst);
        let Some(entry) = work.get(index) else {
            break;
        };
        if effects.stop_requested() {
            if !stopped.swap(true, SeqCst) {
                effects.say("migration_stop_requested=true");
            }
            break;
        }
        match process_entry(effects, &state, entry, index, total, pc) {
            Ok(outcome) => outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((index, outcome)),
            Err(error) => {
                halted.store(true, SeqCst);
                fatal
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert(error);
                break;
            }
        }
    };
    if workers == 1 {
        worker();
    } else {
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(worker);
            }
        });
    }
    if let Some(error) = fatal.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(error);
    }
    summary.stopped = stopped.into_inner();
    let mut outcomes = outcomes
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    // Deterministic summary (unknown names in work order) at any concurrency.
    outcomes.sort_by_key(|(index, _)| *index);
    for (_, outcome) in outcomes {
        match outcome {
            Outcome::SwitchedNow => summary.switched_now += 1,
            Outcome::AlreadySwitched => summary.already_switched += 1,
            Outcome::Lost => summary.lost += 1,
            Outcome::Unknown(name) => summary.unknown.push(name),
            Outcome::ClaimedElsewhere => summary.claimed_elsewhere += 1,
        }
    }
    Ok(summary)
}

/// One archive, start to finish: fingerprint check -> claim -> build ->
/// verified -> switch -> switched (or the resume shortcut of each state).
/// `Err` is reserved for failures that are not about this archive.
fn process_entry(
    effects: &dyn Effects,
    state: &BTreeMap<String, Progress>,
    entry: &Entry,
    index: usize,
    total: usize,
    pc: &str,
) -> Result<Outcome> {
    let label = format!(
        "Migration {}/{}: {} {}",
        index + 1,
        total,
        entry.original_name,
        match entry.action {
            Action::Relocate => "relocate",
            _ => "reencode",
        }
    );
    match state.get(&entry.archive_id) {
        Some(Progress::Switched(_)) => {
            effects.say(&format!("{label} already switched"));
            return Ok(Outcome::AlreadySwitched);
        }
        Some(Progress::Lost(_)) => {
            effects.say(&format!("{label} lost (recorded earlier)"));
            return Ok(Outcome::Lost);
        }
        Some(Progress::Verified(v)) => {
            let (Some(id), Some(location)) = (v.new_archive_id.clone(), v.new_manifest.clone())
            else {
                bail!(
                    "verified record without replacement for {}",
                    entry.archive_id
                );
            };
            effects.say(&format!("{label} re-verify {id}"));
            match effects.reverify(entry, &id, &location) {
                Ok(()) => {
                    let replacement = Replacement {
                        new_archive_id: id,
                        new_manifest: location,
                    };
                    return match switch(effects, entry, &replacement, &v.attempt_id, pc) {
                        Ok(()) => Ok(Outcome::SwitchedNow),
                        Err(e) => unknown(effects, entry, &v.attempt_id, pc, e),
                    };
                }
                Err(e) => {
                    effects.say(&format!(
                        "{label} recorded replacement failed re-verification ({e:#}); rebuilding"
                    ));
                }
            }
        }
        Some(Progress::Claimed(c))
            if c.pc_id != pc
                && !effects.take_over()
                && effects.now().saturating_sub(c.ts_unix) < CLAIM_LEASE_SECONDS =>
        {
            effects.say(&format!("{label} claimed by {}; skipped", c.pc_id));
            return Ok(Outcome::ClaimedElsewhere);
        }
        Some(Progress::Claimed(c)) => {
            // Stale or our own interrupted attempt: its partial copy is an orphan.
            if let Some(id) = &c.new_archive_id {
                let attempt = effects.new_id()?;
                let mut r = record(
                    effects,
                    &entry.archive_id,
                    RecordState::Orphan,
                    &attempt,
                    pc,
                );
                r.new_archive_id = Some(id.clone());
                r.detail = Some(format!(
                    "interrupted attempt {} by {}",
                    c.attempt_id, c.pc_id
                ));
                effects.append(&r)?;
            }
        }
        Some(Progress::Unknown(_)) | None => {}
    }

    // Fresh attempt.
    match effects.source_fingerprint(entry) {
        Ok(fp) if fp == entry.fingerprint => {}
        Ok(_) => {
            let e = anyhow!("source manifest changed since planning; create a new plan");
            return unknown(effects, entry, "", pc, e);
        }
        Err(e) => {
            return unknown(
                effects,
                entry,
                "",
                pc,
                e.context("source manifest unreadable"),
            );
        }
    }
    let attempt = effects.new_id()?;
    let new_archive_id = format!("migrate-{}", effects.new_id()?);
    let mut claim = record(
        effects,
        &entry.archive_id,
        RecordState::Claimed,
        &attempt,
        pc,
    );
    claim.new_archive_id = Some(new_archive_id.clone());
    effects.append(&claim)?;
    effects.say(&format!("{label} -> {new_archive_id}"));
    let replacement = match effects.build(entry, &new_archive_id) {
        Ok(r) => r,
        Err(e) => {
            if let Some(Unrecoverable(losses)) = e.downcast_ref::<Unrecoverable>() {
                let mut r = record(effects, &entry.archive_id, RecordState::Lost, &attempt, pc);
                r.losses = losses.clone();
                r.new_archive_id = Some(new_archive_id.clone());
                r.detail = Some(format!("{e:#}"));
                effects.append(&r)?;
                effects.say(&format!("{label} lost: {e}"));
                return Ok(Outcome::Lost);
            }
            return unknown(effects, entry, &attempt, pc, e);
        }
    };
    let mut verified = record(
        effects,
        &entry.archive_id,
        RecordState::Verified,
        &attempt,
        pc,
    );
    verified.new_archive_id = Some(replacement.new_archive_id.clone());
    verified.new_manifest = Some(replacement.new_manifest.clone());
    effects.append(&verified)?;
    match switch(effects, entry, &replacement, &attempt, pc) {
        Ok(()) => Ok(Outcome::SwitchedNow),
        Err(e) => unknown(effects, entry, &attempt, pc, e),
    }
}

fn switch(
    effects: &dyn Effects,
    entry: &Entry,
    replacement: &Replacement,
    attempt: &str,
    pc: &str,
) -> Result<()> {
    effects.switch(entry, replacement)?;
    let mut r = record(
        effects,
        &entry.archive_id,
        RecordState::Switched,
        attempt,
        pc,
    );
    r.new_archive_id = Some(replacement.new_archive_id.clone());
    r.new_manifest = Some(replacement.new_manifest.clone());
    r.detail = Some(format!(
        "replaces {}; the original is kept",
        entry.archive_id
    ));
    effects.append(&r)?;
    effects.say(&format!(
        "switched {} -> {}",
        entry.archive_id, replacement.new_archive_id
    ));
    Ok(())
}

/// Records `error` as this entry's provider problem. Only the append of the
/// Unknown record itself can fail the run.
fn unknown(
    effects: &dyn Effects,
    entry: &Entry,
    attempt: &str,
    pc: &str,
    error: anyhow::Error,
) -> Result<Outcome> {
    let attempt = if attempt.is_empty() {
        effects.new_id()?
    } else {
        attempt.to_string()
    };
    let mut r = record(
        effects,
        &entry.archive_id,
        RecordState::Unknown,
        &attempt,
        pc,
    );
    r.detail = Some(format!("{error:#}"));
    effects.append(&r)?;
    effects.say(&format!(
        "migration_entry_unknown={} error={error:#}",
        entry.archive_id
    ));
    Ok(Outcome::Unknown(entry.original_name.clone()))
}

// ---------------------------------------------------------------------------
// Real side effects.

struct LiveEffects<'a> {
    rclone: &'a str,
    journal: &'a Journal,
    plan: &'a Plan,
    work_root: PathBuf,
    stop_file: Option<PathBuf>,
    take_over: bool,
    /// Serializes switches: the inventory read-modify-write and the local
    /// replacement log. Cheap next to a build, and it does not rely on the
    /// inventory file lock excluding threads of the same process.
    switch_lock: Mutex<()>,
}

impl LiveEffects<'_> {
    fn load_source(&self, entry: &Entry) -> Result<Manifest> {
        let manifest = crate::manifest::load_manifest(self.rclone, &entry.source)?;
        crate::manifest::validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

impl Effects for LiveEffects<'_> {
    fn append(&self, record: &Record) -> Result<()> {
        self.journal.append(record)
    }
    fn source_fingerprint(&self, entry: &Entry) -> Result<String> {
        crate::manifest::manifest_fingerprint(&self.load_source(entry)?)
    }
    fn build(&self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
        let manifest = self.load_source(entry)?;
        if crate::manifest::manifest_fingerprint(&manifest)? != entry.fingerprint {
            bail!("source manifest changed since planning");
        }
        let work = self.work_root.join(new_archive_id);
        fs::create_dir_all(&work)?;
        let (produced, locations) = match entry.action {
            Action::Relocate => {
                let r = super::relocate::relocate(
                    self.rclone,
                    &manifest,
                    &self.plan.target,
                    new_archive_id,
                    &work,
                )?;
                // Tagged with the archive: several archives run at once.
                println!(
                    "  relocated: downloaded {}, uploaded {} ({})",
                    crate::presentation::format_bytes(r.downloaded_bytes),
                    crate::presentation::format_bytes(r.uploaded_bytes),
                    entry.original_name
                );
                (r.manifest, r.manifest_locations)
            }
            Action::Reencode => crate::pool::reencode_manifest(
                self.rclone,
                &entry.source,
                &manifest,
                &self.plan.target,
                &work,
                new_archive_id,
            )?,
            other => bail!("entry {} is not movable ({other:?})", entry.archive_id),
        };
        if produced.archive_id != new_archive_id || produced.original_size != entry.size {
            bail!("replacement identity/size mismatch");
        }
        let new_manifest = locations
            .into_iter()
            .next()
            .context("replacement has no manifest replica")?;
        // Best effort: the local copy is only a cache; failure is harmless.
        let _ = fs::remove_dir_all(&work);
        Ok(Replacement {
            new_archive_id: new_archive_id.to_string(),
            new_manifest,
        })
    }
    fn reverify(&self, entry: &Entry, new_archive_id: &str, new_manifest: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(self.rclone, new_manifest)?;
        crate::manifest::validate_manifest(&manifest)?;
        if manifest.archive_id != new_archive_id || manifest.original_size != entry.size {
            bail!("replacement manifest identity/size mismatch");
        }
        Ok(())
    }
    fn switch(&self, entry: &Entry, replacement: &Replacement) -> Result<()> {
        let _serialized = self
            .switch_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::inventory::add_manifest(self.rclone, &replacement.new_manifest)?;
        note_replacement(&self.plan.migration_id, entry, replacement)
    }
    fn take_over(&self) -> bool {
        self.take_over
    }
    fn stop_requested(&self) -> bool {
        self.stop_file.as_deref().is_some_and(Path::exists)
    }
}

/// Appends `old -> new` to the local replacement log
/// (`<config>/migrations/replacements.jsonl`) so the user and the GUI can see
/// which archive supersedes which. The old archive stays indexed and readable.
fn note_replacement(migration_id: &str, entry: &Entry, replacement: &Replacement) -> Result<()> {
    let dir = crate::config::app_config_dir()?.join("migrations");
    fs::create_dir_all(&dir)?;
    let line = serde_json::to_string(&serde_json::json!({
        "migration_id": migration_id,
        "old_archive_id": entry.archive_id,
        "old_manifest": entry.source,
        "new_archive_id": replacement.new_archive_id,
        "new_manifest": replacement.new_manifest,
        "original_name": entry.original_name,
        "ts_unix": crate::utils::now_unix(),
    }))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("replacements.jsonl"))?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) mod tests_support {
    use super::super::model::{Action, Counts, Entry, Plan, Record, RecordState};

    pub(crate) fn entry(id: &str, action: Action, size: u64) -> Entry {
        Entry {
            archive_id: id.into(),
            original_name: format!("{id}.bin"),
            size,
            source: format!("r:{id}/manifest.json"),
            fingerprint: format!("fp-{id}"),
            action,
            download_bytes: 0,
            upload_bytes: 0,
            losses: vec![],
            detail: None,
        }
    }

    pub(crate) fn plan(entries: Vec<Entry>) -> Plan {
        Plan {
            version: 1,
            migration_id: "mig".into(),
            pool: "p".into(),
            created_unix: 100,
            created_by: "pc1".into(),
            target: crate::models::PoolDefinition::default(),
            entries,
            counts: Counts::default(),
            download_bytes: 0,
            upload_bytes: 0,
            new_storage_bytes: 0,
            estimated_seconds: None,
            download_mib_s: None,
            upload_mib_s: None,
            quota_ok: None,
            notes: vec![],
        }
    }

    pub(crate) fn rec(entry: &str, state: RecordState, ts: u64, pc: &str) -> Record {
        Record {
            entry: entry.into(),
            state,
            attempt_id: format!("att-{entry}-{ts}"),
            pc_id: pc.into(),
            ts_unix: ts,
            new_archive_id: matches!(
                state,
                RecordState::Verified | RecordState::Switched | RecordState::Claimed
            )
            .then(|| format!("new-{entry}-{ts}")),
            new_manifest: matches!(state, RecordState::Verified | RecordState::Switched)
                .then(|| format!("t:new-{entry}-{ts}/manifest.json")),
            losses: vec![],
            detail: None,
        }
    }
}
