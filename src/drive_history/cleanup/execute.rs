//! One cleanup run over a [`CleanupIo`] (faked in tests).
//!
//! 1. Read the journal and observe everything fresh; any unreadable
//!    reference source postpones the whole run (nothing written).
//! 2. Preview (default): report candidates, waiting and due data.
//! 3. `confirm`: mark new candidates (grace period), release marks whose
//!    data is referenced again, and for due data: guard check, journal
//!    `Deleting`, observe again, delete what is still unreferenced (manifest
//!    replicas first, then shards), journal `Deleted`. An interrupted
//!    deletion resumes on the next run of any PC.
use super::records::{self, Info, Record, State, Step};
use super::select::{select, Archive, Selection, World};
use crate::drive_history::model::{
    CleanupAccount, CleanupMode, CleanupReport, CleanupSettings, CleanupTotals, Retention,
    HISTORY_VERSION,
};
use crate::migration::retire::guard;
use crate::prelude::*;

/// Mass-delete guard: share of the drive's stored bytes/objects one run may delete.
pub(crate) const MAX_PERCENT: u32 = 50;
/// Mass-delete guard: objects one run may delete.
pub(crate) const MAX_OBJECTS: u64 = 10_000;
/// Archives per `Deleted` progress record.
const PROGRESS_BATCH: usize = 100;

pub(crate) trait CleanupIo {
    /// Cleanup records of every PC (fresh).
    fn records(&self) -> Result<Vec<Record>>;
    fn publish(&self, record: &Record) -> Result<()>;
    /// Fresh histories, references and listings (read-only).
    fn observe(&self) -> Result<World>;
    /// Deletes one object; one already gone is not an error.
    fn delete(&self, address: &str) -> Result<()>;
    fn now(&self) -> u64;
    fn new_id(&self) -> Result<String> {
        crate::migration::execute::random_hex(12)
    }
    fn worker(&self) -> String;
    /// The mount is shutting down: stop between archives (resumed later).
    fn stopped(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    pub confirm: bool,
    pub force: bool,
    pub cancel: bool,
    pub retention: Retention,
    pub settings: CleanupSettings,
    pub max_percent: u32,
    pub max_objects: u64,
}
impl Options {
    pub(crate) fn new(retention: Retention, settings: CleanupSettings) -> Self {
        Self {
            confirm: false,
            force: false,
            cancel: false,
            retention,
            settings,
            max_percent: MAX_PERCENT,
            max_objects: MAX_OBJECTS,
        }
    }
}

fn report(pool: &str, mode: CleanupMode, settings: CleanupSettings) -> CleanupReport {
    CleanupReport {
        version: HISTORY_VERSION,
        pool: pool.into(),
        mode,
        candidates: CleanupTotals::default(),
        waiting: CleanupTotals::default(),
        due: CleanupTotals::default(),
        deleted: CleanupTotals::default(),
        released: 0,
        next_deletion_unix: None,
        accounts: Vec::new(),
        guard: None,
        postponed: Vec::new(),
        settings,
        notes: Vec::new(),
    }
}

fn add(totals: &mut CleanupTotals, archive: &Archive) {
    totals.archives += 1;
    totals.files += u64::from(archive.version);
    totals.objects += archive.objects.len() as u64;
    totals.bytes += archive.bytes();
}

fn add_info(totals: &mut CleanupTotals, info: &Info) {
    totals.archives += 1;
    totals.objects += info.objects;
    totals.bytes += info.bytes;
}

fn info(archive: &Archive) -> Info {
    Info {
        objects: archive.objects.len() as u64,
        bytes: archive.bytes(),
    }
}

fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn account(root: &str) -> String {
    root.split_once(':')
        .map_or(root, |(name, _)| name)
        .to_owned()
}

/// Classification of the fresh selection against the journal.
#[derive(Default)]
struct Plan {
    new: BTreeMap<String, Archive>,
    waiting: BTreeMap<String, Archive>,
    /// Due marks and interrupted deletions: deleted by a confirmed run.
    due: BTreeMap<String, Archive>,
    /// Marked but referenced again: released by a confirmed run.
    release: BTreeMap<String, State>,
    /// Deletion started earlier, every object already gone: finished.
    finish: BTreeSet<String>,
    /// Deletion started earlier but referenced again: kept, reported.
    blocked: BTreeSet<String>,
    next: Option<u64>,
}

fn plan(selection: &Selection, journal: &BTreeMap<String, State>, now: u64) -> Plan {
    let mut out = Plan::default();
    for (name, archive) in &selection.candidates {
        let state = journal.get(name);
        match state {
            Some(s) if s.deleting => {
                out.due.insert(name.clone(), archive.clone());
            }
            Some(s) if s.due().is_some_and(|due| due <= now) => {
                out.due.insert(name.clone(), archive.clone());
            }
            Some(s) if s.due().is_some() => {
                out.next = earliest(out.next, s.due());
                out.waiting.insert(name.clone(), archive.clone());
            }
            _ => {
                out.new.insert(name.clone(), archive.clone());
            }
        }
    }
    for (name, state) in journal {
        if state.deleted || selection.candidates.contains_key(name) {
            continue;
        }
        if state.deleting {
            if selection.gone.contains(name) {
                out.finish.insert(name.clone());
            } else {
                out.blocked.insert(name.clone());
            }
        } else if !state.marks.is_empty() {
            out.release.insert(name.clone(), state.clone());
        }
    }
    if !out.due.is_empty() {
        out.next = Some(now);
    }
    out
}

pub(crate) fn run(io: &dyn CleanupIo, pool: &str, options: &Options) -> Result<CleanupReport> {
    let now = io.now();
    let journal = records::fold(&io.records()?);
    if options.cancel {
        return cancel(io, pool, options, &journal, now);
    }
    let world = io.observe()?;
    let selection = select(&world, &options.retention, now);
    let mut out = report(pool, CleanupMode::Preview, options.settings);
    if !selection.uncertain.is_empty() {
        out.mode = CleanupMode::Postponed;
        out.postponed = selection.uncertain;
        for state in journal.values().filter(|s| !s.deleted) {
            add_info(&mut out.waiting, &state.info);
        }
        out.next_deletion_unix = journal.values().filter_map(State::due).min();
        out.notes.push(
            "some references could not be read: nothing is marked or deleted until every source is readable".into(),
        );
        return Ok(out);
    }
    let plan = plan(&selection, &journal, now);
    summarize(&mut out, &plan);
    out.guard = guard_reason(&plan.due, selection.totals, options);
    if !plan.blocked.is_empty() {
        out.notes.push(format!(
            "{} archive(s) whose deletion had started are referenced again and are kept; files using them may be incomplete",
            plan.blocked.len()
        ));
    }
    if !options.confirm {
        return Ok(out);
    }
    out.mode = CleanupMode::Applied;
    let worker = io.worker();
    if !plan.new.is_empty() {
        let mut record = Record::new(
            Step::Mark,
            &io.new_id()?,
            plan.new.iter().map(|(n, a)| (n.clone(), info(a))).collect(),
            &worker,
            now,
        );
        record.grace_seconds =
            u64::from(options.settings.grace_days) * super::super::retention::DAY;
        io.publish(&record)?;
        out.next_deletion_unix = earliest(out.next_deletion_unix, Some(now + record.grace_seconds));
    }
    release(io, &plan.release, &worker, now, &mut out)?;
    if !plan.finish.is_empty() {
        let archives = plan
            .finish
            .iter()
            .map(|n| (n.clone(), Info::default()))
            .collect();
        io.publish(&Record::new(
            Step::Deleted,
            &io.new_id()?,
            archives,
            &worker,
            now,
        ))?;
    }
    if plan.due.is_empty() {
        return Ok(out);
    }
    if out.guard.is_some() && !options.force {
        out.notes.push("due data was not deleted: it exceeds the mass-delete guard (use --force after checking)".into());
        return Ok(out);
    }
    delete(io, &plan.due, &journal, options, &worker, &mut out)?;
    Ok(out)
}

fn summarize(out: &mut CleanupReport, plan: &Plan) {
    let mut accounts: BTreeMap<String, CleanupAccount> = BTreeMap::new();
    for (set, totals) in [
        (&plan.new, &mut out.candidates),
        (&plan.waiting, &mut out.waiting),
        (&plan.due, &mut out.due),
    ] {
        for archive in set.values() {
            add(totals, archive);
            for object in &archive.objects {
                let name = account(&object.root);
                let entry = accounts.entry(name.clone()).or_insert(CleanupAccount {
                    account: name,
                    objects: 0,
                    bytes: 0,
                });
                entry.objects += 1;
                entry.bytes += object.size;
            }
        }
    }
    out.accounts = accounts.into_values().collect();
    out.next_deletion_unix = plan.next;
}

fn guard_reason(
    due: &BTreeMap<String, Archive>,
    totals: guard::Totals,
    options: &Options,
) -> Option<String> {
    guard::check(
        due.values().flat_map(|a| a.objects.iter()),
        totals,
        options.max_percent,
        options.max_objects,
    )
}

/// Releases marks whose data is referenced again (nothing was deleted).
fn release(
    io: &dyn CleanupIo,
    release: &BTreeMap<String, State>,
    worker: &str,
    now: u64,
    out: &mut CleanupReport,
) -> Result<()> {
    let mut by_mark: BTreeMap<&str, BTreeMap<String, Info>> = BTreeMap::new();
    for (name, state) in release {
        for mark in state.marks.keys() {
            by_mark
                .entry(mark)
                .or_default()
                .insert(name.clone(), Info::default());
        }
    }
    for (mark, archives) in by_mark {
        io.publish(&Record::new(Step::Cancel, mark, archives, worker, now))?;
    }
    out.released += release.len() as u64;
    Ok(())
}

fn delete(
    io: &dyn CleanupIo,
    due: &BTreeMap<String, Archive>,
    journal: &BTreeMap<String, State>,
    options: &Options,
    worker: &str,
    out: &mut CleanupReport,
) -> Result<()> {
    let run_id = io.new_id()?;
    let now = io.now();
    let names: BTreeMap<String, Info> = due.iter().map(|(n, a)| (n.clone(), info(a))).collect();
    io.publish(&Record::new(Step::Deleting, &run_id, names, worker, now))?;
    // Re-check every reference right before deleting.
    let fresh = select(&io.observe()?, &options.retention, io.now());
    if !fresh.uncertain.is_empty() {
        out.postponed = fresh.uncertain;
        out.notes.push("the re-check before deleting could not read every reference: deletion resumes on a later run".into());
        return Ok(());
    }
    let mut keep_again = BTreeMap::new();
    let mut targets = Vec::new();
    for name in due.keys() {
        match fresh.candidates.get(name) {
            Some(archive) => targets.push((name.clone(), archive.clone())),
            None if fresh.gone.contains(name) => targets.push((
                name.clone(),
                Archive {
                    objects: vec![],
                    version: due[name].version,
                },
            )),
            None => {
                keep_again.insert(name.clone(), Info::default());
            }
        }
    }
    if !keep_again.is_empty() {
        // Only archives this run started may be released: an interrupted
        // earlier deletion may already have removed objects.
        let (ours, earlier): (BTreeMap<_, _>, BTreeMap<_, _>) = keep_again
            .into_iter()
            .partition(|(n, _)| journal.get(n).is_none_or(|s| !s.deleting));
        if !ours.is_empty() {
            let mut cancel = Record::new(Step::Cancel, &run_id, ours.clone(), worker, io.now());
            cancel.release_deleting = true;
            io.publish(&cancel)?;
            // Their marks stay pending too: release them like any re-reference.
            let states: BTreeMap<String, State> = ours
                .keys()
                .filter_map(|n| journal.get(n).map(|s| (n.clone(), s.clone())))
                .collect();
            release(io, &states, worker, io.now(), out)?;
        }
        if !earlier.is_empty() {
            out.notes.push(format!(
                "{} archive(s) whose deletion had started are referenced again and are kept",
                earlier.len()
            ));
        }
    }
    let mut done = BTreeMap::new();
    for (name, archive) in targets {
        if io.stopped() {
            out.notes
                .push("stopped before the end; the next cleanup run resumes the deletion".into());
            break;
        }
        for object in &archive.objects {
            if let Err(error) = io.delete(&object.address) {
                flush(io, &mut done, worker)?;
                return Err(error.context(format!(
                    "deleting {} stopped; the next cleanup run resumes it",
                    object.address
                )));
            }
        }
        add(&mut out.deleted, &archive);
        done.insert(name, info(&archive));
        if done.len() >= PROGRESS_BATCH {
            flush(io, &mut done, worker)?;
        }
    }
    flush(io, &mut done, worker)
}

fn flush(io: &dyn CleanupIo, done: &mut BTreeMap<String, Info>, worker: &str) -> Result<()> {
    if done.is_empty() {
        return Ok(());
    }
    let archives = std::mem::take(done);
    io.publish(&Record::new(
        Step::Deleted,
        &io.new_id()?,
        archives,
        worker,
        io.now(),
    ))
}

/// Drops every pending mark (deletions already started go on).
fn cancel(
    io: &dyn CleanupIo,
    pool: &str,
    options: &Options,
    journal: &BTreeMap<String, State>,
    now: u64,
) -> Result<CleanupReport> {
    let mut out = report(pool, CleanupMode::Cancelled, options.settings);
    let pending: BTreeMap<String, State> = journal
        .iter()
        .filter(|(_, s)| !s.deleting && !s.deleted && !s.marks.is_empty())
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    let started = journal.values().filter(|s| s.deleting).count();
    release(io, &pending, &io.worker(), now, &mut out)?;
    if started > 0 {
        out.notes.push(format!(
            "{started} archive(s) are already being deleted; that cannot be cancelled"
        ));
    }
    if options.settings.auto {
        out.notes.push("automatic cleanup marks unreferenced data again on its next daily run; turn it off with `rpool drive retention set --auto-cleanup false`".into());
    }
    Ok(out)
}
