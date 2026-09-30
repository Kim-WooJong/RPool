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

#[derive(Debug, Clone, Default)]
pub(crate) struct RunOptions {
    pub stop_file: Option<PathBuf>,
    /// Treat other PCs' unexpired claims as abandoned (after a PC crashed).
    pub take_over: bool,
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
    let mut effects = LiveEffects {
        rclone,
        journal: &journal,
        plan: &plan,
        work_root,
        stop_file: options.stop_file.clone(),
        take_over: options.take_over,
    };
    let summary = run_core(&plan, &records, &pc_id(), &mut effects)?;
    summary.finish()
}

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

pub(crate) trait Effects {
    fn append(&mut self, record: &Record) -> Result<()>;
    /// Fingerprint of the entry's current source manifest (Err: provider error).
    fn source_fingerprint(&mut self, entry: &Entry) -> Result<String>;
    /// Builds and fully verifies the replacement (relocate or re-encode).
    /// An [`Unrecoverable`] error means lost; any other error is unknown.
    fn build(&mut self, entry: &Entry, new_archive_id: &str) -> Result<Replacement>;
    /// Quick check that a recorded replacement manifest loads and validates.
    fn reverify(&mut self, entry: &Entry, new_archive_id: &str, new_manifest: &str) -> Result<()>;
    /// Makes the replacement visible (inventory + replacement note).
    fn switch(&mut self, entry: &Entry, replacement: &Replacement) -> Result<()>;
    fn stop_requested(&self) -> bool;
    /// Other PCs' unexpired claims may be taken over (the user asked for it).
    fn take_over(&self) -> bool {
        false
    }
    fn now(&self) -> u64 {
        crate::utils::now_unix()
    }
    fn new_id(&mut self) -> Result<String> {
        random_hex(12)
    }
    fn say(&mut self, line: &str) {
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
    effects: &mut dyn Effects,
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

/// The resume/run state machine over a frozen plan and the journal records.
pub(crate) fn run_core(
    plan: &Plan,
    records: &[Record],
    pc: &str,
    effects: &mut dyn Effects,
) -> Result<RunSummary> {
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
    for (index, entry) in work.into_iter().enumerate() {
        let label = format!(
            "Migration {}/{}: {} {}",
            index + 1,
            summary.total,
            entry.original_name,
            match entry.action {
                Action::Relocate => "relocate",
                _ => "reencode",
            }
        );
        if effects.stop_requested() {
            summary.stopped = true;
            effects.say("migration_stop_requested=true");
            break;
        }
        let current = state.get(&entry.archive_id);
        match current {
            Some(Progress::Switched(_)) => {
                summary.already_switched += 1;
                effects.say(&format!("{label} already switched"));
                continue;
            }
            Some(Progress::Lost(_)) => {
                summary.lost += 1;
                effects.say(&format!("{label} lost (recorded earlier)"));
                continue;
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
                        match switch(effects, entry, &replacement, &v.attempt_id, pc) {
                            Ok(()) => summary.switched_now += 1,
                            Err(e) => unknown(effects, &mut summary, entry, &v.attempt_id, pc, e)?,
                        }
                        continue;
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
                summary.claimed_elsewhere += 1;
                effects.say(&format!("{label} claimed by {}; skipped", c.pc_id));
                continue;
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
                unknown(effects, &mut summary, entry, "", pc, e)?;
                continue;
            }
            Err(e) => {
                unknown(
                    effects,
                    &mut summary,
                    entry,
                    "",
                    pc,
                    e.context("source manifest unreadable"),
                )?;
                continue;
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
                    summary.lost += 1;
                    effects.say(&format!("{label} lost: {e}"));
                } else {
                    unknown(effects, &mut summary, entry, &attempt, pc, e)?;
                }
                continue;
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
            Ok(()) => summary.switched_now += 1,
            Err(e) => unknown(effects, &mut summary, entry, &attempt, pc, e)?,
        }
    }
    Ok(summary)
}

fn switch(
    effects: &mut dyn Effects,
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

fn unknown(
    effects: &mut dyn Effects,
    summary: &mut RunSummary,
    entry: &Entry,
    attempt: &str,
    pc: &str,
    error: anyhow::Error,
) -> Result<()> {
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
    summary.unknown.push(entry.original_name.clone());
    Ok(())
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
}

impl LiveEffects<'_> {
    fn load_source(&self, entry: &Entry) -> Result<Manifest> {
        let manifest = crate::manifest::load_manifest(self.rclone, &entry.source)?;
        crate::manifest::validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

impl Effects for LiveEffects<'_> {
    fn append(&mut self, record: &Record) -> Result<()> {
        self.journal.append(record)
    }
    fn source_fingerprint(&mut self, entry: &Entry) -> Result<String> {
        crate::manifest::manifest_fingerprint(&self.load_source(entry)?)
    }
    fn build(&mut self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
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
                println!(
                    "  relocated: downloaded {}, uploaded {}",
                    crate::presentation::format_bytes(r.downloaded_bytes),
                    crate::presentation::format_bytes(r.uploaded_bytes)
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
    fn reverify(&mut self, entry: &Entry, new_archive_id: &str, new_manifest: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(self.rclone, new_manifest)?;
        crate::manifest::validate_manifest(&manifest)?;
        if manifest.archive_id != new_archive_id || manifest.original_size != entry.size {
            bail!("replacement manifest identity/size mismatch");
        }
        Ok(())
    }
    fn switch(&mut self, entry: &Entry, replacement: &Replacement) -> Result<()> {
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
