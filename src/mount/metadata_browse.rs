//! Incremental, workspace-less reading of one pool metadata generation (the
//! Library on a PC where the pool is not mounted). A [`Snapshot`] of the
//! validated events and checkpoint chunks read before lets the next read list
//! every replica but fetch only records it has not seen.
//!
//! Safety: records are content addressed and immutable, so a cached event
//! whose id is the hash of its content is the same event the cloud holds. A
//! snapshot is only used when every cached event is still listed by a
//! replica or covered by a usable checkpoint; anything else (a replaced
//! namespace, missing checkpointed records) falls back to a full read.
use super::metadata_cache::Cache;
use super::metadata_checkpoint::{pull, Replica};
use super::metadata_checkpoint_model::Family;
use super::metadata_limits::{PAGE_BYTES, PAGE_RECORDS};
use super::metadata_pool::{replica_dirs, ReplicaDirs};
use super::pool_sync::{list_unseen_parallel, EventStore, Unseen};
use super::shared_model::Event;
use super::shared_transport::SharedTransport;
use crate::prelude::*;

/// Records fetched at the same time.
const READ_PARALLEL: usize = 8;
/// Records fetched per page (raw bytes of one page at most are held).
const READ_PAGE: usize = 256;

/// What a previous read of a generation established.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    /// Every event of the generation (listed and checkpointed), by id.
    pub events: BTreeMap<String, Event>,
    /// Checkpoint chunks already ingested (their records are in `events`).
    pub checkpoints: Cache,
}

impl Snapshot {
    /// The snapshot when every event is valid under its content id and
    /// every ingested checkpoint record is present; an error otherwise.
    pub(crate) fn validated(self) -> Result<Self> {
        for (id, event) in &self.events {
            event.validate()?;
            if event.id()? != *id {
                bail!("cached metadata event identity mismatch");
            }
        }
        let complete = self
            .checkpoints
            .chunks
            .values()
            .flat_map(|kinds| kinds.values())
            .flatten()
            .all(|id| self.events.contains_key(id));
        if !complete {
            bail!("cached checkpoint records missing");
        }
        Ok(self)
    }
}

/// The result of [`Source::refresh`].
pub(crate) struct Outcome {
    /// The refreshed snapshot.
    pub snapshot: Snapshot,
    /// The snapshot differs from the one passed in.
    pub changed: bool,
    /// The previous snapshot was not usable and everything was read again.
    pub full: bool,
}

/// Replicas of one generation of a saved pool.
pub(crate) struct Source {
    /// One shared transport per replica root.
    stores: Vec<SharedTransport>,
    /// Checkpoint/record directories of each replica, in `stores` order.
    dirs: Vec<ReplicaDirs>,
}

/// Family roots of a generation: `<root>` or `<root>/epochs/<epoch>`.
pub(crate) fn generation_roots(
    pool: &str,
    policy: &PoolDefinition,
    epoch: Option<&str>,
) -> Result<Vec<String>> {
    Ok(super::pool_sync::roots(pool, &policy.remotes)?
        .into_iter()
        .map(|root| match epoch {
            Some(epoch) => crate::utils::remote_join(&root, &format!("epochs/{epoch}")),
            None => root,
        })
        .collect())
}

impl Source {
    /// Opens transports and replica directories for every root of the generation `epoch`
    /// (`None`: the original generation).
    pub(crate) fn open(
        rclone: &str,
        pool: &str,
        policy: &PoolDefinition,
        epoch: Option<&str>,
    ) -> Result<Self> {
        let roots = generation_roots(pool, policy, epoch)?;
        let stores = roots
            .iter()
            .map(|root| {
                SharedTransport::new(rclone, root).map(|t| t.with_native_crypt(policy.native_crypt))
            })
            .collect::<Result<_>>()?;
        let dirs = replica_dirs(rclone, &roots, policy.native_crypt, &Family::v6())?;
        Ok(Self { stores, dirs })
    }

    /// Every record id of every replica (listing only, all replicas at once).
    pub(crate) fn list(&self) -> Result<Unseen> {
        let stores: Vec<&(dyn EventStore + Sync)> = self.stores.iter().map(|s| s as _).collect();
        list_unseen_parallel(&stores, &BTreeSet::new())
    }

    /// Brings `prior` up to date with `listed` (from [`Source::list`]).
    pub(crate) fn refresh(&self, listed: &Unseen, prior: Snapshot) -> Result<Outcome> {
        let replicas: Vec<_> = self.dirs.iter().map(ReplicaDirs::replica).collect();
        let read = |unseen: &Unseen, skip: &dyn Fn(&str) -> bool| {
            read_parallel(&self.stores, unseen, skip)
        };
        refresh_with(listed, &replicas, prior, &read)
    }
}

/// Fetches listed records not satisfied by `skip`; injectable for tests.
type Reader<'a> = dyn Fn(&Unseen, &dyn Fn(&str) -> bool) -> Result<BTreeMap<String, Event>> + 'a;

/// [`Source::refresh`] over any replicas; `read` fetches listed records not
/// satisfied by its `skip`.
pub(crate) fn refresh_with(
    listed: &Unseen,
    replicas: &[Replica<'_>],
    prior: Snapshot,
    read: &Reader<'_>,
) -> Result<Outcome> {
    if !prior.events.is_empty() {
        if let Some(outcome) = advance(listed, replicas, prior, read)? {
            return Ok(outcome);
        }
    }
    let mut outcome = advance(listed, replicas, Snapshot::default(), read)?
        .context("a full metadata read cannot be inconsistent")?;
    outcome.full = true;
    outcome.changed = true;
    Ok(outcome)
}

/// One pass; `None` when `prior` does not match the cloud.
fn advance(
    listed: &Unseen,
    replicas: &[Replica<'_>],
    prior: Snapshot,
    read: &Reader<'_>,
) -> Result<Option<Outcome>> {
    let family = Family::v6();
    let Snapshot {
        mut events,
        checkpoints: mut cache,
    } = prior;
    let bootstrap = events.is_empty();
    if bootstrap {
        cache = Cache::default();
    }
    let before = (events.len(), cache.clone());
    let mut incoming = BTreeMap::new();
    let covered = pull(
        &family,
        replicas,
        &mut cache,
        listed.gate,
        bootstrap,
        &mut |_, id, text| {
            if !events.contains_key(id) && !incoming.contains_key(id) {
                let event: Event = serde_json::from_str(text)?;
                event.validate()?;
                if event.id()? != id {
                    bail!("checkpoint event identity mismatch");
                }
                incoming.insert(id.to_owned(), event);
            }
            Ok(())
        },
    )?;
    let covered = covered.get("events").cloned().unwrap_or_default();
    // A cached record neither listed nor checkpointed: the namespace is not
    // the one the snapshot was taken from.
    if !bootstrap
        && events
            .keys()
            .any(|id| !listed.entries.contains_key(id) && !covered.contains(id))
    {
        return Ok(None);
    }
    let tail = read(listed, &|id| {
        events.contains_key(id) || incoming.contains_key(id)
    })?;
    events.extend(incoming);
    events.extend(tail);
    if !bootstrap && covered.iter().any(|id| !events.contains_key(id)) {
        return Ok(None);
    }
    let changed = events.len() != before.0 || cache != before.1;
    Ok(Some(Outcome {
        snapshot: Snapshot {
            events,
            checkpoints: cache,
        },
        changed,
        full: false,
    }))
}

/// Reads the listed records not satisfied by `skip`, [`READ_PARALLEL`] at a
/// time, page by page; every record is verified against its id.
fn read_parallel<S: EventStore + Sync>(
    stores: &[S],
    unseen: &Unseen,
    skip: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Event>> {
    let wanted: Vec<(&str, usize, u64)> = unseen
        .entries
        .iter()
        .filter(|(id, _)| !skip(id))
        .map(|(id, (index, size))| (id.as_str(), *index, *size))
        .collect();
    let mut events = BTreeMap::new();
    let mut start = 0;
    while start < wanted.len() {
        let mut end = start;
        let mut bytes = 0u64;
        while end < wanted.len()
            && end - start < READ_PAGE.min(PAGE_RECORDS)
            && (end == start || bytes + wanted[end].2 <= PAGE_BYTES as u64)
        {
            bytes += wanted[end].2;
            end += 1;
        }
        let page = &wanted[start..end];
        let next = std::sync::atomic::AtomicUsize::new(0);
        let mut fetched: Vec<(usize, Result<Vec<u8>>)> = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..READ_PARALLEL.min(page.len()))
                .map(|_| {
                    let next = &next;
                    scope.spawn(move || {
                        let mut done = Vec::new();
                        loop {
                            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some((id, index, size)) = page.get(at) else {
                                break done;
                            };
                            let bytes = stores[*index].read(id, *size);
                            let failed = bytes.is_err();
                            done.push((at, bytes));
                            if failed {
                                break done;
                            }
                        }
                    })
                })
                .collect();
            jobs.into_iter()
                .flat_map(|job| {
                    job.join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        });
        fetched.sort_by_key(|(at, _)| *at);
        for (at, bytes) in fetched {
            let id = page[at].0;
            let event: Event = serde_json::from_slice(&bytes?)?;
            event.validate()?;
            if event.id()? != id {
                bail!("pool metadata event identity mismatch");
            }
            events.insert(id.to_owned(), event);
        }
        start = end;
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Thread-safe in-memory replica (the checkpoint fakes are not `Sync`).
    #[derive(Default)]
    struct Store {
        events: Mutex<BTreeMap<String, Vec<u8>>>,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl EventStore for Store {
        fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| !known.contains(*id))
                .map(|(id, b)| (id.clone(), b.clone()))
                .collect())
        }
        fn publish(&self, _: &str, _: &[u8]) -> Result<()> {
            bail!("read-only")
        }
        fn read(&self, id: &str, _: u64) -> Result<Vec<u8>> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.events.lock().unwrap().get(id).cloned().context("gone")
        }
    }
    fn event(i: usize) -> (String, Vec<u8>) {
        let e = Event {
            version: 1,
            worker: "w".into(),
            device: "d".into(),
            path: format!("f{i}"),
            parents: vec![],
            content: None,
        };
        (e.id().unwrap(), serde_json::to_vec(&e).unwrap())
    }

    #[test]
    fn parallel_listing_and_reads_match_the_sequential_ones() {
        let stores = [Store::default(), Store::default()];
        for i in 0..600 {
            let (id, bytes) = event(i);
            stores[i % 2].events.lock().unwrap().insert(id, bytes);
        }
        let shared: Vec<&(dyn EventStore + Sync)> = stores.iter().map(|s| s as _).collect();
        let plain: Vec<&dyn EventStore> = stores.iter().map(|s| s as _).collect();
        let listed = list_unseen_parallel(&shared, &BTreeSet::new()).unwrap();
        let sequential = super::super::pool_sync::list_unseen(&plain, &BTreeSet::new()).unwrap();
        assert_eq!(listed.entries, sequential.entries);
        let skipped = event(0).0;
        let events = read_parallel(&stores, &listed, &|id| id == skipped).unwrap();
        assert_eq!(events.len(), 599);
        assert!(!events.contains_key(&skipped));
        let reads: usize = stores
            .iter()
            .map(|s| s.reads.load(std::sync::atomic::Ordering::Relaxed))
            .sum();
        assert_eq!(reads, 599);
        // A record that changed under its id is refused.
        let (id, _) = event(7);
        let holder = &stores[7 % 2];
        holder.events.lock().unwrap().insert(id.clone(), event(8).1);
        assert!(read_parallel(&stores, &listed, &|_| false).is_err());
    }

    #[test]
    fn snapshots_round_trip_and_tampering_is_rejected() {
        let mut snapshot = Snapshot::default();
        for i in 0..3 {
            let (id, bytes) = event(i);
            snapshot
                .events
                .insert(id, serde_json::from_slice(&bytes).unwrap());
        }
        let text = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back.validated().unwrap().events.len(), 3);
        let mut tampered: Snapshot = serde_json::from_str(&text).unwrap();
        tampered.events.values_mut().next().unwrap().path = "other".into();
        assert!(tampered.validated().is_err());
        let mut orphan: Snapshot = serde_json::from_str(&text).unwrap();
        orphan.checkpoints.chunks.insert(
            "c".repeat(64),
            [("events".into(), ["f".repeat(64)].into())].into(),
        );
        assert!(orphan.validated().is_err());
    }
}
