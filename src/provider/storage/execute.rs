//! One cleanup run over a `StorageIo` (faked in tests), with the drive
//! cleanup's rules (`drive_history::cleanup::execute`):
//!
//! 1. Read the journal and classify everything fresh; any unreadable source
//!    postpones the whole run (nothing written).
//! 2. Every selected group must exist, be deletable and have its
//!    acknowledgements; otherwise the run is refused.
//! 3. Preview (default): units to mark, waiting and due.
//! 4. `confirm`: mark new units (grace period; speed-test leftovers have
//!    none), release marks of units that are needed again, and for due
//!    units: mass-delete guard (`force` passes it), journal `Deleting`,
//!    classify again, delete what is still deletable in a selected group
//!    (manifest replicas first, then the other objects; metadata
//!    generations only after every archive only they name is gone), remove
//!    emptied folders, journal `Deleted`. An interrupted deletion resumes on
//!    the next run of any PC.
use super::classify::{Classified, Placed};
use super::model::{Ack, CleanMode, CleanReport, Group, StorageView, Totals, VERSION};
use crate::drive_history::cleanup::records::{self, Info, Record, State, Step};
use crate::migration::retire::guard;
use crate::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Mass-delete guard: share of the provider's stored bytes/objects one run may delete.
pub(crate) const MAX_PERCENT: u32 = crate::drive_history::cleanup::execute::MAX_PERCENT;
/// Mass-delete guard: objects one run may delete.
pub(crate) const MAX_OBJECTS: u64 = crate::drive_history::cleanup::execute::MAX_OBJECTS;
/// Units per `Deleted` progress record (and per deletion batch).
const BATCH: usize = 100;
/// Units per published record (keeps records far below the metadata size limit).
const RECORD_UNITS: usize = 2_000;

/// Everything a run reads and writes. `live::LiveIo` is the cloud side.
pub(crate) trait StorageIo: Sync {
    /// Provider root (`remote:path`).
    fn root(&self) -> String;
    /// Journal records of every PC (fresh).
    fn records(&self) -> Result<Vec<Record>>;
    /// Publishes one journal record.
    fn publish(&self, record: &Record) -> Result<()>;
    /// Fresh read-only observation, classified.
    fn observe(&self) -> Result<Classified>;
    /// Deletes one object; one already gone is not an error.
    fn delete(&self, address: &str) -> Result<()>;
    /// Removes `address` if it is an empty folder on a backend that keeps
    /// empty folders (SFTP, SMB, …); best effort, never fails.
    fn remove_dir(&self, address: &str);
    /// Current unix time (seconds).
    fn now(&self) -> u64;
    /// A fresh random id for a mark or deletion run.
    fn new_id(&self) -> Result<String> {
        crate::migration::execute::random_hex(12)
    }
    /// Name of this PC, stored in published records.
    fn worker(&self) -> String;
    /// Days between marking and deleting.
    fn grace_days(&self) -> u32;
    /// Parallel deletions.
    fn workers(&self) -> usize {
        1
    }
}

/// What a cleanup run may do (`provider storage clean`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Request {
    /// Selected group ids.
    pub groups: Vec<String>,
    /// Mark and delete (default: preview).
    pub confirm: bool,
    /// Delete past the mass-delete guard.
    pub force: bool,
    /// Drop every pending mark.
    pub cancel: bool,
    /// `--include-unconfirmed-generations`.
    pub unconfirmed: bool,
    /// `--include-foreign-pools`.
    pub foreign: bool,
    /// Parallel deletions.
    pub workers: usize,
}

impl Request {
    /// Whether `ack` was given.
    fn has(&self, ack: Ack) -> bool {
        match ack {
            Ack::UnconfirmedGenerations => self.unconfirmed,
            Ack::ForeignPools => self.foreign,
        }
    }
    /// Whether `group` may be deleted by this request.
    fn allows(&self, group: &Group) -> bool {
        group.deletable && group.needs.iter().all(|a| self.has(*a))
    }
}

/// Size of a unit, as recorded in the journal.
fn info(placed: &Placed) -> Info {
    Info {
        objects: placed.objects.len() as u64,
        bytes: placed.bytes(),
    }
}

/// Adds a unit to `totals`.
fn add(totals: &mut Totals, placed: &Placed) {
    totals.units += 1;
    totals.objects += placed.objects.len() as u64;
    totals.bytes += placed.bytes();
}

/// The earlier of two optional times.
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The grouped view with each group's pending marks (`provider storage
/// list`). A journal that cannot be read only adds a note.
pub(crate) fn view(
    classified: &Classified,
    records: Result<Vec<Record>>,
    grace_days: u32,
    undecryptable: Option<u64>,
) -> StorageView {
    let mut notes = Vec::new();
    let journal = match records {
        Ok(records) => records::fold(&records),
        Err(error) => {
            notes.push(format!("pending marks could not be read: {error:#}"));
            BTreeMap::new()
        }
    };
    let mut groups = classified.groups.clone();
    for (key, state) in &journal {
        if state.deleted {
            continue;
        }
        let Some(group) = classified
            .units
            .get(key)
            .and_then(|p| groups.get_mut(&p.group))
        else {
            continue;
        };
        let pending = &mut group.pending;
        if state.deleting {
            pending.deleting += 1;
        }
        if let Some(due) = state.due() {
            pending.marked += 1;
            pending.bytes += state.info.bytes;
            pending.first_due_unix = earliest(pending.first_due_unix, Some(due));
            pending.last_due_unix = pending.last_due_unix.max(Some(due));
        }
    }
    let mut groups: Vec<Group> = groups.into_values().collect();
    groups.sort_by(|a, b| (a.deletable, a.kind, &a.id).cmp(&(b.deletable, b.kind, &b.id)));
    if !classified.uncertain.is_empty() {
        notes.push(
            "some sources could not be read: nothing can be deleted until every source is readable"
                .into(),
        );
    }
    StorageView {
        version: VERSION,
        remote: classified.root.clone(),
        pools: classified.pools.clone(),
        groups,
        objects: classified.totals.objects,
        bytes: classified.totals.bytes,
        uncertain: classified.uncertain.clone(),
        undecryptable,
        grace_days,
        notes,
    }
}

/// Publishes `units` in records of at most [`RECORD_UNITS`] units.
fn publish(
    io: &dyn StorageIo,
    step: Step,
    mark: &str,
    units: BTreeMap<String, Info>,
    grace_seconds: u64,
    release_deleting: bool,
) -> Result<()> {
    let units: Vec<(String, Info)> = units.into_iter().collect();
    for chunk in units.chunks(RECORD_UNITS) {
        let mut record = Record::new(
            step,
            mark,
            chunk.iter().cloned().collect(),
            &io.worker(),
            io.now(),
        );
        record.grace_seconds = grace_seconds;
        record.release_deleting = release_deleting;
        io.publish(&record)?;
    }
    Ok(())
}

/// Releases the pending marks of `states` (nothing was deleted).
fn release(
    io: &dyn StorageIo,
    states: &BTreeMap<String, State>,
    out: &mut CleanReport,
) -> Result<()> {
    let mut by_mark: BTreeMap<&str, BTreeMap<String, Info>> = BTreeMap::new();
    for (key, state) in states {
        for mark in state.marks.keys() {
            by_mark
                .entry(mark)
                .or_default()
                .insert(key.clone(), Info::default());
        }
    }
    for (mark, units) in by_mark {
        publish(io, Step::Cancel, mark, units, 0, false)?;
    }
    out.released += states.len() as u64;
    Ok(())
}

/// Selected units against the journal.
#[derive(Default)]
struct Plan {
    /// Not marked yet.
    new: BTreeMap<String, Placed>,
    /// Not marked yet and deleted without grace (speed-test leftovers).
    immediate: BTreeMap<String, Placed>,
    /// Marked, grace period not over (or metadata waiting for its archives).
    waiting: BTreeMap<String, Placed>,
    /// Due marks and interrupted deletions.
    due: BTreeMap<String, Placed>,
    /// Marked, but no longer deletable: released by a confirmed run.
    release: BTreeMap<String, State>,
    /// Deletion started earlier and every object is gone.
    finish: BTreeSet<String>,
    /// Deletion started earlier but no longer deletable: kept, reported.
    blocked: BTreeSet<String>,
    /// Metadata units waiting for archives that are not deleted with them.
    blocked_metadata: u64,
    /// Earliest time a waiting mark becomes due.
    next: Option<u64>,
}

/// Whether metadata unit `placed` may go together with `deleted` (archive
/// unit keys): every archive it waits for is gone or among them.
fn ready(placed: &Placed, units: &BTreeMap<String, Placed>, deleted: &BTreeSet<String>) -> bool {
    placed
        .depends
        .iter()
        .all(|key| !units.contains_key(key) || deleted.contains(key))
}

/// Classifies the selected units of `world` against `journal` at `now`.
fn plan(
    world: &Classified,
    selected: &BTreeSet<String>,
    journal: &BTreeMap<String, State>,
    now: u64,
) -> Plan {
    let mut out = Plan::default();
    for (key, placed) in &world.units {
        if !selected.contains(&placed.group) {
            continue;
        }
        let immediate = world.groups[&placed.group].immediate;
        match journal.get(key) {
            Some(s) if s.deleted => {}
            Some(s) if s.deleting => {
                out.due.insert(key.clone(), placed.clone());
            }
            Some(s) if s.due().is_some_and(|due| due <= now) => {
                out.due.insert(key.clone(), placed.clone());
            }
            Some(s) if s.due().is_some() => {
                out.next = earliest(out.next, s.due());
                out.waiting.insert(key.clone(), placed.clone());
            }
            _ if immediate => {
                out.immediate.insert(key.clone(), placed.clone());
            }
            _ => {
                out.new.insert(key.clone(), placed.clone());
            }
        }
    }
    // Metadata goes only with (or after) every archive it waits for.
    let deleting: BTreeSet<String> = out
        .due
        .iter()
        .filter(|(_, p)| !p.metadata)
        .map(|(k, _)| k.clone())
        .collect();
    let waiting: Vec<String> = out
        .due
        .iter()
        .filter(|(_, p)| p.metadata && !ready(p, &world.units, &deleting))
        .map(|(k, _)| k.clone())
        .collect();
    for key in waiting {
        if let Some(placed) = out.due.remove(&key) {
            out.blocked_metadata += 1;
            out.waiting.insert(key, placed);
        }
    }
    for (key, state) in journal {
        if state.deleted {
            continue;
        }
        let deletable = world.group_of(key).is_some_and(|g| g.deletable);
        let present = world.units.contains_key(key);
        if state.deleting {
            if !present {
                out.finish.insert(key.clone());
            } else if !deletable {
                out.blocked.insert(key.clone());
            }
        } else if !state.marks.is_empty() && (!present || !deletable) {
            out.release.insert(key.clone(), state.clone());
        }
    }
    out
}

/// One cleanup run (see the module docs). Called by `storage::clean`.
pub(crate) fn run(io: &dyn StorageIo, request: &Request) -> Result<CleanReport> {
    let grace_days = io.grace_days();
    let mut out = CleanReport::new(&io.root(), CleanMode::Preview, &request.groups, grace_days);
    let now = io.now();
    let journal = records::fold(&io.records()?);
    if request.cancel {
        return cancel(io, &journal, out);
    }
    if request.groups.is_empty() {
        bail!("choose at least one group (--group <id>); `rpool provider storage list` shows them");
    }
    let world = io.observe()?;
    if !world.uncertain.is_empty() {
        out.mode = CleanMode::Postponed;
        out.postponed = world.uncertain.clone();
        out.notes.push(
            "some sources could not be read: nothing is marked or deleted until every source is readable"
                .into(),
        );
        return Ok(out);
    }
    let selected = check_selection(&world, request)?;
    let plan = plan(&world, &selected, &journal, now);
    for (set, totals) in [
        (&plan.new, &mut out.new),
        (&plan.waiting, &mut out.waiting),
        (&plan.due, &mut out.due),
    ] {
        set.values().for_each(|p| add(totals, p));
    }
    plan.immediate.values().for_each(|p| add(&mut out.due, p));
    let due_now = !plan.due.is_empty() || !plan.immediate.is_empty();
    out.next_deletion_unix = if due_now { Some(now) } else { plan.next };
    if plan.blocked_metadata > 0 {
        out.notes.push(format!(
            "{} metadata generation(s) wait until every archive only they name is deleted",
            plan.blocked_metadata
        ));
    }
    if !plan.blocked.is_empty() {
        out.notes.push(format!(
            "{} unit(s) whose deletion had started are needed again and are kept; data using them may be incomplete",
            plan.blocked.len()
        ));
    }
    let due: BTreeMap<String, Placed> = plan
        .due
        .iter()
        .chain(&plan.immediate)
        .map(|(k, p)| (k.clone(), p.clone()))
        .collect();
    out.guard = guard::check(
        due.values().flat_map(|p| p.objects.iter()),
        world.totals,
        MAX_PERCENT,
        MAX_OBJECTS,
    );
    if !request.confirm {
        return Ok(out);
    }
    out.mode = CleanMode::Applied;
    // What stays marked after this run (due data is deleted below unless refused).
    out.next_deletion_unix = plan.next;
    if !plan.new.is_empty() {
        let grace = u64::from(grace_days) * crate::drive_history::retention::DAY;
        let units = plan.new.iter().map(|(k, p)| (k.clone(), info(p))).collect();
        publish(io, Step::Mark, &io.new_id()?, units, grace, false)?;
        out.next_deletion_unix = earliest(out.next_deletion_unix, Some(now + grace));
    }
    if !plan.immediate.is_empty() {
        let units = plan
            .immediate
            .iter()
            .map(|(k, p)| (k.clone(), info(p)))
            .collect();
        publish(io, Step::Mark, &io.new_id()?, units, 0, false)?;
    }
    release(io, &plan.release, &mut out)?;
    if !plan.finish.is_empty() {
        let units = plan
            .finish
            .iter()
            .map(|k| (k.clone(), Info::default()))
            .collect();
        publish(io, Step::Deleted, &io.new_id()?, units, 0, false)?;
    }
    if due.is_empty() {
        return Ok(out);
    }
    if out.guard.is_some() && !request.force {
        out.next_deletion_unix = Some(now);
        out.notes.push(
            "due data was not deleted: it exceeds the mass-delete guard (use --force after checking)"
                .into(),
        );
        return Ok(out);
    }
    delete(io, request, &selected, &due, &journal, &mut out)
}

/// The selected group ids, after checking each exists, may be deleted and
/// has its acknowledgements.
fn check_selection(world: &Classified, request: &Request) -> Result<BTreeSet<String>> {
    let mut selected = BTreeSet::new();
    for id in &request.groups {
        let group = world.groups.get(id).with_context(|| {
            format!(
                "no group {id} on this provider; `rpool provider storage list` shows the groups"
            )
        })?;
        if !group.deletable {
            bail!("group {id} is kept: {}", group.reason);
        }
        if let Some(ack) = group.needs.iter().find(|a| !request.has(**a)) {
            bail!(
                "group {id} is deleted only with {}: {}",
                ack.flag(),
                group.reason
            );
        }
        selected.insert(id.clone());
    }
    Ok(selected)
}

/// Deletes the due units: journal `Deleting`, classify again, delete what is
/// still deletable (archives first, then metadata), journal `Deleted`.
fn delete(
    io: &dyn StorageIo,
    request: &Request,
    selected: &BTreeSet<String>,
    due: &BTreeMap<String, Placed>,
    journal: &BTreeMap<String, State>,
    out: &mut CleanReport,
) -> Result<CleanReport> {
    let run_id = io.new_id()?;
    let units = due.iter().map(|(k, p)| (k.clone(), info(p))).collect();
    publish(io, Step::Deleting, &run_id, units, 0, false)?;
    // Only this run's own `Deleting` may be undone: an earlier, interrupted
    // deletion may already have removed objects.
    let ours = |key: &String| journal.get(key).is_none_or(|s| !s.deleting);
    // Releases this run's `Deleting` (nothing deleted yet).
    let undo_all = || {
        let undo = due
            .keys()
            .filter(|k| ours(k))
            .map(|k| (k.clone(), Info::default()))
            .collect();
        publish(io, Step::Cancel, &run_id, undo, 0, true)
    };
    let fresh = match io.observe() {
        Ok(fresh) => fresh,
        Err(error) => {
            let error = error.context("the re-check before deleting failed: nothing was deleted");
            return Err(match undo_all() {
                Ok(()) => error,
                Err(undo) => {
                    error.context(format!("releasing this run's deletion failed: {undo:#}"))
                }
            });
        }
    };
    if !fresh.uncertain.is_empty() {
        undo_all()?;
        out.postponed = fresh.uncertain;
        out.notes.push(
            "the re-check before deleting could not read every source: nothing was deleted".into(),
        );
        return Ok(out.clone());
    }
    let mut archives = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut gone = BTreeSet::new();
    let mut needed = BTreeMap::new();
    for key in due.keys() {
        match fresh.units.get(key) {
            None => {
                gone.insert(key.clone());
            }
            Some(p)
                if selected.contains(&p.group)
                    && fresh
                        .groups
                        .get(&p.group)
                        .is_some_and(|g| request.allows(g)) =>
            {
                let target = if p.metadata {
                    &mut metadata
                } else {
                    &mut archives
                };
                target.insert(key.clone(), p.clone());
            }
            Some(_) => {
                needed.insert(key.clone(), Info::default());
            }
        }
    }
    let deleting: BTreeSet<String> = archives.keys().cloned().collect();
    let waiting: BTreeMap<String, Info> = metadata
        .iter()
        .filter(|(_, p)| !ready(p, &fresh.units, &deleting))
        .map(|(k, _)| (k.clone(), Info::default()))
        .collect();
    metadata.retain(|k, _| !waiting.contains_key(k));
    // Waiting metadata keeps its mark; only this run's `Deleting` is undone.
    let undo: BTreeMap<String, Info> = needed
        .iter()
        .chain(&waiting)
        .filter(|(k, _)| ours(k))
        .map(|(k, i)| (k.clone(), *i))
        .collect();
    if !undo.is_empty() {
        publish(io, Step::Cancel, &run_id, undo, 0, true)?;
    }
    let release_now: BTreeMap<String, State> = needed
        .keys()
        .filter(|k| ours(k))
        .filter_map(|k| journal.get(k).map(|s| (k.clone(), s.clone())))
        .collect();
    release(io, &release_now, out)?;
    let earlier = needed.keys().filter(|k| !ours(k)).count();
    if earlier > 0 {
        out.notes.push(format!(
            "{earlier} unit(s) whose deletion had started are needed again and are kept"
        ));
    }
    if !waiting.is_empty() {
        out.notes.push(format!(
            "{} metadata generation(s) wait until every archive only they name is deleted",
            waiting.len()
        ));
    }
    if !gone.is_empty() {
        let units = gone.iter().map(|k| (k.clone(), Info::default())).collect();
        publish(io, Step::Deleted, &io.new_id()?, units, 0, false)?;
    }
    let mut emptied = BTreeSet::new();
    let root = io.root();
    for set in [&archives, &metadata] {
        let keys: Vec<&String> = set.keys().collect();
        for batch in keys.chunks(BATCH) {
            let placed: Vec<&Placed> = batch.iter().map(|k| &set[*k]).collect();
            let (manifests, rest): (Vec<&str>, Vec<&str>) = placed
                .iter()
                .flat_map(|p| p.objects.iter().map(|o| o.address.as_str()))
                .partition(|a| a.ends_with("/manifest.json"));
            delete_all(io, &manifests, request.workers.max(io.workers()))
                .and_then(|()| delete_all(io, &rest, request.workers.max(io.workers())))
                .map_err(|e| e.context("deletion stopped; the next cleanup run resumes it"))?;
            let mut done = BTreeMap::new();
            for (key, p) in batch.iter().zip(&placed) {
                add(&mut out.deleted, p);
                done.insert((*key).clone(), info(p));
                for object in &p.objects {
                    if let Ok(rel) = crate::utils::relative_remote_object(&root, &object.address) {
                        emptied.extend(super::layout::parents(&rel).map(str::to_owned));
                    }
                }
            }
            publish(io, Step::Deleted, &io.new_id()?, done, 0, false)?;
        }
    }
    // Deepest folders first; only empty ones are removed.
    let mut folders: Vec<String> = emptied.into_iter().collect();
    folders.sort_by_key(|f| std::cmp::Reverse((f.matches('/').count(), f.clone())));
    for folder in folders {
        io.remove_dir(&crate::utils::remote_join(&root, &folder));
    }
    Ok(out.clone())
}

/// Deletes `addresses` with `workers` threads; the first error stops the rest.
fn delete_all(io: &dyn StorageIo, addresses: &[&str], workers: usize) -> Result<()> {
    if addresses.is_empty() {
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let stop = || failed.lock().map(|f| f.is_some()).unwrap_or(true);
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, addresses.len()) {
            scope.spawn(|| {
                while !stop() {
                    let Some(address) = addresses.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if let Err(error) = io.delete(address) {
                        if let Ok(mut slot) = failed.lock() {
                            slot.get_or_insert(error.context(format!("deleting {address}")));
                        }
                        break;
                    }
                }
            });
        }
    });
    match failed.into_inner() {
        Ok(None) => Ok(()),
        Ok(Some(error)) => Err(error),
        Err(_) => bail!("a deletion worker panicked"),
    }
}

/// Drops every pending mark of this provider (deletions already started go on).
fn cancel(
    io: &dyn StorageIo,
    journal: &BTreeMap<String, State>,
    mut out: CleanReport,
) -> Result<CleanReport> {
    out.mode = CleanMode::Cancelled;
    let pending: BTreeMap<String, State> = journal
        .iter()
        .filter(|(_, s)| !s.deleting && !s.deleted && !s.marks.is_empty())
        .map(|(k, s)| (k.clone(), s.clone()))
        .collect();
    release(io, &pending, &mut out)?;
    let started = journal.values().filter(|s| s.deleting).count();
    if started > 0 {
        out.notes.push(format!(
            "{started} unit(s) are already being deleted; that cannot be cancelled"
        ));
    }
    Ok(out)
}
