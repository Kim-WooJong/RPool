//! RS-specific coordinator: workers write only private staging files. Only the
//! coordinator writes output/commits the journal, including after hedge wins.
use super::*;
use crate::erasure::reconstruct_group;
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::storage::reader::{check_read_context, is_restore_unavailable, ReadProgress};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Timing policy for speculative (hedged) parity reads.
pub(super) struct Policy {
    /// Stall/slow threshold used until enough samples exist or when not adaptive.
    pub delay: Duration,
    /// How long the coordinator waits for a worker completion per loop turn.
    pub tick: Duration,
    /// Derive the threshold from recent shard timings instead of using `delay`.
    pub adaptive: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(2),
            tick: Duration::from_millis(50),
            adaptive: true,
        }
    }
}
impl Policy {
    /// Stall threshold: `delay`, or twice the median of the last samples
    /// (normalized per full shard) clamped to 250 ms..30 s once 4 samples exist.
    fn threshold(&self, samples: &VecDeque<Duration>) -> Duration {
        if !self.adaptive || samples.len() < 4 {
            return self.delay;
        }
        let mut sorted: Vec<_> = samples.iter().copied().collect();
        sorted.sort();
        (sorted[sorted.len() / 2] * 2).clamp(Duration::from_millis(250), Duration::from_secs(30))
    }
}

#[derive(Clone)]
/// One queued shard download.
struct Task {
    /// Shard to download.
    shard: Shard,
    /// 1-based attempt number; retries stop at the `retries` limit.
    attempt: u32,
    /// Earliest start time (retry backoff).
    ready: Instant,
    /// Hedge read for a slow group; counts against the hedge budget.
    speculative: bool,
}
/// A download currently handed to a worker, keyed by shard index.
struct Active {
    /// The task being executed.
    task: Task,
    /// Private staging file the worker writes.
    path: PathBuf,
    /// Set by the coordinator to cancel the read cooperatively.
    cancel: Arc<AtomicBool>,
    /// Byte progress used to detect stalled or slow reads.
    progress: Arc<ReadProgress>,
}
/// Restore state of one Reed-Solomon coding group (at most two are open).
struct Group {
    /// Data shards of the group.
    data: Vec<Shard>,
    /// Parity shards not yet requested.
    candidates: VecDeque<Shard>,
    /// Parity reads cancelled as slow; retried once alternatives are exhausted.
    deferred: VecDeque<Shard>,
    /// Data reads cancelled as slow; retried when parity cannot cover them.
    deferred_data: VecDeque<Shard>,
    /// Data shard indexes on their final protected retry (never cancelled as slow again).
    protected: BTreeSet<u32>,
    /// Parity shard indexes on their final protected retry.
    protected_parity: BTreeSet<u32>,
    /// Verified parity inputs: coding slot -> staged file.
    parity: BTreeMap<usize, PathBuf>,
    /// Data shard indexes that failed or were given up as slow.
    failed: BTreeSet<u32>,
    /// Staging directory for this group's shard files.
    temp: tempfile::TempDir,
    /// Group fully restored (directly or reconstructed).
    done: bool,
}
/// Message sent to a worker thread.
struct Work {
    /// Shard index; identifies the completion.
    id: u32,
    /// Shard to download.
    shard: Shard,
    /// Staging file to write.
    path: PathBuf,
    /// Cancellation flag shared with the coordinator.
    cancel: Arc<AtomicBool>,
    /// Progress counter shared with the coordinator.
    progress: Arc<ReadProgress>,
}

/// Restores an erasure-coded archive into `output`. Workers only download into
/// staging files; this coordinator commits data, reconstructs a group once the
/// verified parity covers its missing data, and issues bounded speculative
/// parity reads (hedge budget 1–2 workers) for stalled or slow groups.
/// Resumable through the shared resume state. Called by `get_erasure`.
pub(super) fn restore(
    reader: &StorageReader,
    manifest: &Manifest,
    coding: &Coding,
    output: &Path,
    workers: usize,
    retries: u32,
    policy: Policy,
) -> Result<()> {
    if workers == 0 {
        bail!("workers must be positive");
    }
    let state_path = prepare_output_and_state(manifest, output)?;
    let mut state: ResumeState = read_json(&state_path)?;
    let mut planned = BTreeMap::<u32, (Vec<Shard>, VecDeque<Shard>)>::new();
    for shard in &manifest.shards {
        let (data, parity) = planned.entry(shard.group).or_default();
        if shard.kind == ShardKind::Data {
            data.push(shard.clone());
        } else {
            parity.push_back(shard.clone());
        }
    }
    let mut planned = planned.into_iter();
    let mut exhausted = false;
    let mut groups = BTreeMap::<u32, Group>::new();
    let mut pending = VecDeque::<Task>::new();
    let mut active = BTreeMap::<u32, Active>::new();
    let mut samples = VecDeque::new();
    // Reserve room for speculative progress even if every normal stream stalls.
    // Single-worker operation retains error recovery but cannot race reads.
    let hedge_budget = if workers > 1 {
        (workers / 4).clamp(1, 2)
    } else {
        0
    };
    let normal_limit = workers - hedge_budget;
    let (work_tx, work_rx) = mpsc::sync_channel::<Work>(workers);
    let work_rx = Mutex::new(work_rx);
    let (done_tx, done_rx) = mpsc::channel();
    let mut fatal: Option<anyhow::Error> = None;
    let mut last_remote = String::new();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let rx = &work_rx;
            let tx = done_tx.clone();
            scope.spawn(move || loop {
                let Ok(work) = rx.lock().expect("hedge receiver poisoned").recv() else {
                    break;
                };
                let context = reader.operation_context().child(work.cancel.clone());
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    reader.download_staged(&work.shard, &work.path, &context, &work.progress)
                }))
                .unwrap_or_else(|_| Err(anyhow!("hedged read worker panicked")));
                if tx.send((work.id, result)).is_err() {
                    break;
                }
            });
        }
        loop {
            debug_assert!(active.len() <= workers);
            debug_assert!(groups.len() <= 2);
            debug_assert!(active.values().filter(|a| a.task.speculative).count() <= hedge_budget);
            if fatal.is_none() {
                let step = (|| -> Result<()> {
                    check_read_context(reader.operation_context())?;
                    // A finished group's TempDir outlives every losing worker.
                    groups.retain(|id, g| {
                        !g.done || active.values().any(|a| a.task.shard.group == *id)
                    });
                    while groups.len() < 2 && !exhausted {
                        let Some((id, (data, parity))) = planned.next() else {
                            exhausted = true;
                            break;
                        };
                        if data.iter().all(|s| state.completed.contains(&s.index)) {
                            continue;
                        }
                        for shard in &data {
                            if !state.completed.contains(&shard.index) {
                                pending.push_back(Task {
                                    shard: shard.clone(),
                                    attempt: 1,
                                    ready: Instant::now(),
                                    speculative: false,
                                });
                            }
                        }
                        groups.insert(
                            id,
                            Group {
                                data,
                                candidates: parity,
                                deferred: VecDeque::new(),
                                deferred_data: VecDeque::new(),
                                protected: BTreeSet::new(),
                                protected_parity: BTreeSet::new(),
                                parity: BTreeMap::new(),
                                failed: BTreeSet::new(),
                                temp: tempfile::tempdir()?,
                                done: false,
                            },
                        );
                    }
                    for (id, group) in groups.iter_mut().filter(|(_, g)| !g.done) {
                        let missing: Vec<_> = group
                            .data
                            .iter()
                            .filter(|s| !state.completed.contains(&s.index))
                            .cloned()
                            .collect();
                        // Only real data inputs are counted; absent final-group
                        // slots are supplied as known zeros by the decoder.
                        if missing.len() <= group.parity.len() {
                            for a in active.values().filter(|a| a.task.shard.group == *id) {
                                a.cancel.store(true, Ordering::Release);
                            }
                            pending.retain(|t| t.shard.group != *id);
                            if !missing.is_empty() {
                                let parity: Vec<_> = group
                                    .parity
                                    .iter()
                                    .map(|(slot, p)| (*slot, p.clone()))
                                    .collect();
                                reconstruct_group(
                                    manifest, coding, *id, &missing, output, &parity,
                                )?;
                                for shard in &missing {
                                    state.completed.insert(shard.index);
                                }
                                persist_restore_state(&state_path, &mut state)?;
                                eprintln!("[hedge] group {id} recovered from verified inputs");
                            }
                            group.done = true;
                            continue;
                        }
                        // A verified parity input can stand in for one slow
                        // data input, freeing a normal slot for queued useful data.
                        // Retain the data candidate for a final required retry if
                        // other inputs later fail: slow never means permanently lost.
                        let threshold = policy.threshold(&samples);
                        for a in active.values().filter(|a| {
                            a.task.shard.group == *id && a.task.shard.kind == ShardKind::Data
                        }) {
                            if group.parity.len() > group.failed.len()
                                && !group.protected.contains(&a.task.shard.index)
                                && !a.cancel.load(Ordering::Acquire)
                                && (a.progress.stalled(threshold)
                                    || (samples.len() >= 4
                                        && a.progress.slow(a.task.shard.size, threshold)))
                            {
                                group.failed.insert(a.task.shard.index);
                                group.deferred_data.push_back(a.task.shard.clone());
                                a.cancel.store(true, Ordering::Release);
                            }
                        }
                        let parity_inflight = active
                            .values()
                            .filter(|a| {
                                a.task.shard.group == *id && a.task.shard.kind == ShardKind::Parity
                            })
                            .count()
                            + pending
                                .iter()
                                .filter(|t| {
                                    t.shard.group == *id && t.shard.kind == ShardKind::Parity
                                })
                                .count();
                        let required = group
                            .failed
                            .len()
                            .saturating_sub(group.parity.len() + parity_inflight);
                        for _ in 0..required {
                            if let Some(shard) = take_required(group) {
                                pending.push_back(Task {
                                    shard,
                                    attempt: 1,
                                    ready: Instant::now(),
                                    speculative: false,
                                });
                            }
                        }
                        let threshold = policy.threshold(&samples);
                        // Replace a stalled speculative candidate, but only
                        // after its cooperative worker acknowledges cancellation.
                        if !group.candidates.is_empty()
                            || pending
                                .iter()
                                .any(|t| t.shard.group == *id && t.shard.kind == ShardKind::Parity)
                        {
                            for a in active.values().filter(|a| {
                                a.task.shard.group == *id
                                    && a.task.shard.kind == ShardKind::Parity
                                    && !group.protected_parity.contains(&a.task.shard.index)
                            }) {
                                if a.progress.elapsed() >= threshold * 2
                                    && (a.progress.stalled(threshold)
                                        || (samples.len() >= 4
                                            && a.progress.slow(a.task.shard.size, threshold)))
                                {
                                    a.cancel.store(true, Ordering::Release);
                                }
                            }
                        }
                        let slow = active.values().any(|a| {
                            a.task.shard.group == *id
                                && (a.progress.stalled(threshold)
                                    || (samples.len() >= 4
                                        && a.progress.slow(a.task.shard.size, threshold)))
                        });
                        let spec_count = active.values().filter(|a| a.task.speculative).count()
                            + pending.iter().filter(|t| t.speculative).count();
                        let group_spec = active
                            .values()
                            .any(|a| a.task.shard.group == *id && a.task.speculative)
                            || pending
                                .iter()
                                .any(|t| t.shard.group == *id && t.speculative);
                        if hedge_budget > 0 && slow && !group_spec && spec_count < hedge_budget {
                            if let Some(shard) = take_candidate(&mut group.candidates, &active) {
                                eprintln!("[hedge] requesting parity for slow group {id}");
                                pending.push_front(Task {
                                    shard,
                                    attempt: 1,
                                    ready: Instant::now(),
                                    speculative: true,
                                });
                            }
                        }
                        // A slow candidate is not lost. Once alternatives are
                        // exhausted, retry it once without speculative retirement.
                        if group.candidates.is_empty()
                            && !group.deferred.is_empty()
                            && !active.values().any(|a| {
                                a.task.shard.group == *id && a.task.shard.kind == ShardKind::Parity
                            })
                            && !pending
                                .iter()
                                .any(|t| t.shard.group == *id && t.shard.kind == ShardKind::Parity)
                        {
                            let shard = group.deferred.pop_front().unwrap();
                            group.protected_parity.insert(shard.index);
                            pending.push_back(Task {
                                shard,
                                attempt: 1,
                                ready: Instant::now(),
                                speculative: false,
                            });
                        }
                        if group.candidates.is_empty()
                            && group.deferred.is_empty()
                            && group.failed.len() > group.parity.len()
                            && !active.values().any(|a| {
                                a.task.shard.group == *id && a.task.shard.kind == ShardKind::Parity
                            })
                            && !pending
                                .iter()
                                .any(|t| t.shard.group == *id && t.shard.kind == ShardKind::Parity)
                        {
                            if let Some(pos) = group
                                .deferred_data
                                .iter()
                                .position(|s| !active.contains_key(&s.index))
                            {
                                let shard = group.deferred_data.remove(pos).unwrap();
                                group.failed.remove(&shard.index);
                                group.protected.insert(shard.index);
                                pending.push_back(Task {
                                    shard,
                                    attempt: 1,
                                    ready: Instant::now(),
                                    speculative: false,
                                });
                            }
                        }
                        if !active.values().any(|a| a.task.shard.group == *id)
                            && !pending.iter().any(|t| t.shard.group == *id)
                        {
                            bail!("group {id} has insufficient verified inputs for reconstruction");
                        }
                    }
                    // Round-robin configured remote names; necessary recovery
                    // and one bounded hedge may use reserved transfer capacity.
                    while active.len() < workers {
                        let domains: BTreeSet<_> = pending
                            .iter()
                            .map(|t| scheduler::remote_key(&t.shard.remote))
                            .chain(
                                active
                                    .values()
                                    .map(|a| scheduler::remote_key(&a.task.shard.remote)),
                            )
                            .collect();
                        let cap = if domains.len() <= 1 {
                            workers
                        } else {
                            workers.div_ceil(2)
                        };
                        let mut ring: Vec<_> = domains
                            .iter()
                            .filter(|d| **d > last_remote)
                            .chain(domains.iter().filter(|d| **d <= last_remote))
                            .cloned()
                            .collect();
                        let data_active = active
                            .values()
                            .filter(|a| a.task.shard.kind == ShardKind::Data)
                            .count();
                        let mut selected = None;
                        for domain in ring.drain(..) {
                            if active
                                .values()
                                .filter(|a| scheduler::remote_key(&a.task.shard.remote) == domain)
                                .count()
                                >= cap
                            {
                                continue;
                            }
                            if let Some(i) = pending.iter().position(|t| {
                                t.ready <= Instant::now()
                                    && scheduler::remote_key(&t.shard.remote) == domain
                                    && (t.shard.kind == ShardKind::Parity
                                        || data_active < normal_limit)
                            }) {
                                selected = Some((i, domain));
                                break;
                            }
                        }
                        if selected.is_none() {
                            // Do not strand a reserved hedge slot behind the
                            // normal per-remote cap. Only parity may borrow one
                            // extra slot; global workers remains a hard bound.
                            if let Some(i) = pending.iter().position(|t| {
                                t.ready <= Instant::now()
                                    && t.shard.kind == ShardKind::Parity
                                    && active
                                        .values()
                                        .filter(|a| {
                                            scheduler::remote_key(&a.task.shard.remote)
                                                == scheduler::remote_key(&t.shard.remote)
                                        })
                                        .count()
                                        < (cap + 1).min(workers)
                            }) {
                                selected =
                                    Some((i, scheduler::remote_key(&pending[i].shard.remote)));
                            }
                        }
                        let Some((i, domain)) = selected else {
                            break;
                        };
                        let task = pending.remove(i).unwrap();
                        last_remote = domain;
                        let path = groups[&task.shard.group]
                            .temp
                            .path()
                            .join(format!("{}-{}", task.shard.index, task.attempt));
                        let cancel = Arc::new(AtomicBool::new(false));
                        let progress = Arc::new(ReadProgress::new());
                        let work = Work {
                            id: task.shard.index,
                            shard: task.shard.clone(),
                            path: path.clone(),
                            cancel: cancel.clone(),
                            progress: progress.clone(),
                        };
                        active.insert(
                            work.id,
                            Active {
                                task,
                                path,
                                cancel,
                                progress,
                            },
                        );
                        work_tx
                            .send(work)
                            .map_err(|_| anyhow!("hedge workers disconnected"))?;
                    }
                    Ok(())
                })();
                if let Err(error) = step {
                    fatal = Some(error);
                }
            }
            if fatal.is_some() {
                for a in active.values() {
                    a.cancel.store(true, Ordering::Release);
                }
                pending.clear();
            }
            if active.is_empty()
                && (fatal.is_some() || (exhausted && groups.values().all(|g| g.done)))
            {
                break;
            }
            let Ok((id, result)) = done_rx.recv_timeout(policy.tick) else {
                continue;
            };
            let a = active.remove(&id).expect("registered read completion");
            if fatal.is_some() {
                continue;
            }
            let handled = (|| -> Result<()> {
                check_read_context(reader.operation_context())?;
                let group = groups
                    .get_mut(&a.task.shard.group)
                    .expect("active group retained");
                if group.done {
                    // Ignore only deliberate loser cancellation/unavailability;
                    // auth and local disk errors are never reclassified as loss.
                    if let Err(error) = result {
                        let cancelled = a.cancel.load(Ordering::Acquire)
                            && error
                                .downcast_ref::<StorageError>()
                                .is_some_and(|e| e.kind() == StorageErrorKind::Cancelled);
                        if !cancelled && !is_restore_unavailable(&error) {
                            return Err(error);
                        }
                    }
                    let _ = fs::remove_file(&a.path);
                    return Ok(());
                }
                if a.task.shard.kind == ShardKind::Data && a.cancel.load(Ordering::Acquire) {
                    if let Err(error) = &result {
                        if is_restore_unavailable(error)
                            || error
                                .downcast_ref::<StorageError>()
                                .is_some_and(|e| e.kind() == StorageErrorKind::Cancelled)
                        {
                            return Ok(());
                        }
                    }
                }
                if a.task.shard.kind == ShardKind::Parity && a.cancel.load(Ordering::Acquire) {
                    if let Err(error) = &result {
                        if is_restore_unavailable(error)
                            || error
                                .downcast_ref::<StorageError>()
                                .is_some_and(|e| e.kind() == StorageErrorKind::Cancelled)
                        {
                            group.deferred.push_back(a.task.shard.clone());
                            return Ok(());
                        }
                    }
                }
                match result {
                    Ok(()) => {
                        samples.push_back(Duration::from_secs_f64(
                            (a.progress.elapsed().as_secs_f64() * manifest.shard_size as f64
                                / a.task.shard.size.max(1) as f64)
                                .min(600.0),
                        ));
                        if samples.len() > 16 {
                            samples.pop_front();
                        }
                        if a.task.shard.kind == ShardKind::Data {
                            commit_data(&a.task.shard, &a.path, output)?;
                            group.failed.remove(&id);
                            state.completed.insert(id);
                            persist_restore_state(&state_path, &mut state)?;
                            fs::remove_file(&a.path)?;
                        } else {
                            group.parity.insert(a.task.shard.slot as usize, a.path);
                        }
                    }
                    Err(error) => {
                        if a.task.attempt < retries.max(1) {
                            if let Some(delay) = scheduler::read_retry(&error, a.task.attempt) {
                                let mut task = a.task;
                                task.attempt += 1;
                                task.ready = Instant::now() + delay;
                                pending.push_back(task);
                                return Ok(());
                            }
                        }
                        if !is_restore_unavailable(&error) {
                            return Err(error);
                        }
                        if a.task.shard.kind == ShardKind::Data {
                            group.failed.insert(id);
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = handled {
                fatal = Some(error);
            }
        }
        drop(work_tx);
    });
    if let Some(error) = fatal {
        return Err(error);
    }
    if data_shards(manifest)
        .iter()
        .any(|s| !state.completed.contains(&s.index))
    {
        bail!("incomplete hedged restore");
    }
    fs::remove_file(state_path)?;
    println!("restored={}", output.display());
    Ok(())
}

/// Copies a verified staged data shard into `output` at its offset; fails if
/// the staged file is not exactly the shard size.
fn commit_data(shard: &Shard, staged: &Path, output: &Path) -> Result<()> {
    let mut input = File::open(staged)?;
    let target = OpenOptions::new().read(true).write(true).open(output)?;
    let mut buffer = vec![0; IO_BUFFER];
    let mut copied = 0;
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if copied + n as u64 > shard.size {
            bail!("staged shard grew before commit");
        }
        crate::utils::write_all_at(&target, &buffer[..n], shard.offset + copied)?;
        copied += n as u64;
    }
    if copied != shard.size {
        bail!("staged shard shortened before commit");
    }
    Ok(())
}

/// Removes and returns the parity candidate whose remote has the fewest active reads.
fn take_candidate(
    candidates: &mut VecDeque<Shard>,
    active: &BTreeMap<u32, Active>,
) -> Option<Shard> {
    let index = candidates
        .iter()
        .enumerate()
        .min_by_key(|(_, shard)| {
            let key = scheduler::remote_key(&shard.remote);
            active
                .values()
                .filter(|a| scheduler::remote_key(&a.task.shard.remote) == key)
                .count()
        })
        .map(|(i, _)| i)?;
    candidates.remove(index)
}

/// Next parity input needed to cover a failure: an untried candidate first,
/// else a deferred one, which is then protected from further slow-cancellation.
fn take_required(group: &mut Group) -> Option<Shard> {
    if let Some(shard) = group.candidates.pop_front() {
        return Some(shard);
    }
    let shard = group.deferred.pop_front()?;
    group.protected_parity.insert(shard.index);
    Some(shard)
}
