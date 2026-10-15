//! Automatic immutable metadata replication within the existing encrypted pool.
//! No mutable shared catalog, coordinator, or destructive history collection.
//!
//! Entry points: `roots` (per-remote v6 metadata roots of a pool),
//! `VirtualDrive::pull_pool` / publication of local events, and the listing
//! helpers `list_unseen`/`read_unseen` shared with `metadata_pool` browsing.
use super::folders::{FolderRecord, Records};
use super::metadata_limits::{PAGE_BYTES, PAGE_RECORDS, STREAM_BYTES_MAX, STREAM_RECORDS_MAX};
use super::shared_model::Event;
use super::shared_transport::SharedTransport;
use crate::prelude::*;

/// Pool name defines stable identity within actual pool destinations; membership order
/// and expansion cannot redirect discovery to an empty namespace.
/// The same portable pool/remote-root configuration must be used on each PC.
pub(crate) fn roots(pool: &str, remotes: &[String]) -> Result<Vec<String>> {
    crate::pool::validate_pool_name(pool)?;
    let mut configured = remotes.to_vec();
    configured.sort();
    configured.dedup();
    if configured.is_empty() {
        bail!("pool sync requires at least one encrypted remote");
    }
    let scope = blake3::hash(&serde_json::to_vec(&("rpool-pool-sync-v6", pool))?)
        .to_hex()
        .to_string();
    let mut roots = crate::remote_root::apply_remote_roots(configured)?;
    roots.sort();
    roots.dedup();
    for root in &mut roots {
        *root = crate::utils::remote_join(root, &format!(".rpool-sync/events-v6/{scope}"));
        SharedTransport::new("unused", root)?;
    }
    Ok(roots)
}

/// Namespace events by id (as `collect` returns them).
pub(crate) type Events = BTreeMap<String, Event>;

/// One replica of the pool-sync event directory. Implemented by
/// `SharedTransport` (real remotes); tests use in-memory stores.
pub(crate) trait EventStore {
    /// Every listed event not in `known`, with its bytes.
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>>;
    /// Writes event `bytes` under `id` (content-addressed; idempotent).
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()>;
    /// Listing only: unseen `(id, size)`. The default reads (test fakes).
    fn unseen(&self, known: &BTreeSet<String>) -> Result<Vec<(String, u64)>> {
        Ok(self
            .missing(known)?
            .into_iter()
            .map(|(id, bytes)| (id, bytes.len() as u64))
            .collect())
    }
    /// One listed record, verified. The default reads everything (test fakes).
    fn read(&self, id: &str, _size: u64) -> Result<Vec<u8>> {
        self.missing(&BTreeSet::new())?
            .remove(id)
            .context("pool metadata event vanished during listing")
    }
}
impl EventStore for SharedTransport {
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        self.list_missing(known)
    }
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        SharedTransport::publish(self, id, bytes)
    }
    fn unseen(&self, known: &BTreeSet<String>) -> Result<Vec<(String, u64)>> {
        self.entries(known, None)
    }
    fn read(&self, id: &str, size: u64) -> Result<Vec<u8>> {
        self.read_verified(id, size)
    }
}

/// Unseen records of every replica: id -> (first replica listing it, size).
pub(crate) struct Unseen {
    /// Unseen id -> (index of the first replica listing it, size in bytes).
    pub entries: BTreeMap<String, (usize, u64)>,
    /// The compaction gate record is listed (deletion may have happened).
    pub gate: bool,
}
/// Lists replicas one after another and merges their unseen records;
/// stops at the first unreachable replica. Used by `pull_pool` and
/// `metadata_pool::read_v6`.
pub(crate) fn list_unseen(stores: &[&dyn EventStore], known: &BTreeSet<String>) -> Result<Unseen> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    // Lazy: an unreachable replica stops the listing before the next one.
    merge_unseen(stores.iter().map(|store| store.unseen(known)), known)
}
/// [`list_unseen`] with every replica listed at the same time.
pub(crate) fn list_unseen_parallel(
    stores: &[&(dyn EventStore + Sync)],
    known: &BTreeSet<String>,
) -> Result<Unseen> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    let listings: Vec<_> = std::thread::scope(|scope| {
        let jobs: Vec<_> = stores
            .iter()
            .map(|store| scope.spawn(move || store.unseen(known)))
            .collect();
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    merge_unseen(listings, known)
}
/// Merges per-replica listings in replica order (the first replica listing a
/// record serves it).
fn merge_unseen(
    listings: impl IntoIterator<Item = Result<Vec<(String, u64)>>>,
    known: &BTreeSet<String>,
) -> Result<Unseen> {
    let gate = super::metadata_checkpoint_model::Family::v6().gate_id();
    let mut result = Unseen {
        entries: BTreeMap::new(),
        gate: false,
    };
    let mut bytes = 0u64;
    for (index, listing) in listings.into_iter().enumerate() {
        // List every configured replica. An outage is an error, never an empty namespace.
        for (id, size) in listing.context("Pool metadata read failed; keep the workspace")? {
            if id == gate {
                result.gate = true;
                continue;
            }
            if known.contains(&id) || result.entries.contains_key(&id) {
                continue;
            }
            bytes = bytes.saturating_add(size);
            result.entries.insert(id, (index, size));
            if result.entries.len() > STREAM_RECORDS_MAX || bytes > STREAM_BYTES_MAX {
                bail!(super::metadata_limits::ceiling_message(
                    result.entries.len(),
                    bytes
                ));
            }
        }
    }
    Ok(result)
}
/// Reads the listed records not satisfied by `skip`, page by page: raw bytes
/// of one page at most are held, parsed events accumulate.
pub(crate) fn read_unseen(
    stores: &[&dyn EventStore],
    unseen: &Unseen,
    skip: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, Event>> {
    let mut events = BTreeMap::new();
    let mut page: Vec<(String, Vec<u8>)> = Vec::new();
    let mut page_bytes = 0usize;
    let mut flush = |page: &mut Vec<(String, Vec<u8>)>| -> Result<()> {
        for (id, bytes) in page.drain(..) {
            let event: Event = serde_json::from_slice(&bytes)?;
            event.validate()?;
            if event.id()? != id {
                bail!("pool metadata event identity mismatch");
            }
            events.insert(id, event);
        }
        Ok(())
    };
    for (id, (index, size)) in &unseen.entries {
        if skip(id) {
            continue;
        }
        let bytes = stores[*index].read(id, *size)?;
        page_bytes += bytes.len();
        page.push((id.clone(), bytes));
        if page.len() >= PAGE_RECORDS || page_bytes >= PAGE_BYTES {
            flush(&mut page)?;
            page_bytes = 0;
        }
    }
    flush(&mut page)?;
    Ok(events)
}
#[cfg(test)]
pub(crate) fn collect(
    stores: &[&dyn EventStore],
    known: &BTreeSet<String>,
) -> Result<BTreeMap<String, Event>> {
    let unseen = list_unseen(stores, known)?;
    read_unseen(stores, &unseen, |_| false)
}
#[cfg(test)]
pub(crate) fn replicate(stores: &[&dyn EventStore], id: &str, event: &Event) -> Result<()> {
    if stores.is_empty() || event.id()? != id {
        bail!("invalid metadata publication");
    }
    event.validate()?;
    let bytes = serde_json::to_vec(event)?;
    for store in stores {
        store.publish(id, &bytes)?;
    }
    // Caller acknowledges only after every configured replica verified publication.
    Ok(())
}

/// Events published at the same time by `VirtualDrive::publish_pool`.
const PUBLISH_PARALLEL: usize = 8;

/// One event to every replica at once; all must verify it.
fn publish_everywhere(transports: &[SharedTransport], id: &str, event: &Event) -> Result<()> {
    if event.id()? != id {
        bail!("invalid metadata publication");
    }
    event.validate()?;
    let bytes = serde_json::to_vec(event)?;
    let results: Vec<Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = transports
            .iter()
            .map(|t| {
                let bytes = &bytes;
                scope.spawn(move || EventStore::publish(t, id, bytes))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    results.into_iter().collect()
}

/// Folder records live in a sibling tree unseen by pre-folder clients.
pub(crate) fn folder_transports(
    rclone: &str,
    roots: &[String],
    native_crypt: bool,
) -> Result<Vec<SharedTransport>> {
    roots
        .iter()
        .map(|root| {
            SharedTransport::new(rclone, &crate::utils::remote_join(root, "folders"))
                .map(|t| t.with_native_crypt(native_crypt))
        })
        .collect()
}

pub(crate) fn read_folders(stores: &[&dyn EventStore], unseen: &Unseen) -> Result<Records> {
    let mut records = Records::new();
    for (id, (index, size)) in &unseen.entries {
        let bytes = stores[*index].read(id, *size)?;
        records.insert(id.clone(), super::folders::parse(id, &bytes)?);
    }
    Ok(records)
}

/// Folder metadata must not make an otherwise healthy file pool unusable.
/// Keep strict reads for generation migration, where an incomplete snapshot
/// would silently omit folders from the adopted generation.
pub(crate) fn read_folders_best_effort(stores: &[&dyn EventStore], unseen: &Unseen) -> Records {
    let mut records = Records::new();
    for (id, (index, size)) in &unseen.entries {
        let result = stores[*index]
            .read(id, *size)
            .and_then(|bytes| super::folders::parse(id, &bytes));
        match result {
            Ok(record) => {
                records.insert(id.clone(), record);
            }
            Err(error) => {
                eprintln!("rpool: ignoring unreadable folder record {id}: {error:#}");
            }
        }
    }
    records
}

fn publish_folder_everywhere(
    transports: &[SharedTransport],
    id: &str,
    record: &FolderRecord,
) -> Result<()> {
    if record.id()? != id {
        bail!("invalid folder publication");
    }
    record.validate()?;
    let bytes = serde_json::to_vec(record)?;
    for transport in transports {
        EventStore::publish(transport, id, &bytes)?;
    }
    Ok(())
}

impl super::virtual_drive::VirtualDrive {
    /// One `SharedTransport` per pool-sync root of this drive (native crypt as
    /// the pool configures).
    pub(crate) fn pool_transports(&self) -> Result<Vec<SharedTransport>> {
        self.pool_sync_roots
            .iter()
            .map(|root| {
                SharedTransport::new(&self.rclone, root)
                    .map(|t| t.with_native_crypt(self.policy.native_crypt))
            })
            .collect()
    }
    /// Pulls unseen events (checkpointed and tail records) from the pool into
    /// the namespace and saves it; checkpointed events are marked published.
    /// Called by `virtual_drive::sync` at the start of each sync round.
    pub(crate) fn pull_pool(&self) -> Result<()> {
        let transports = self.pool_transports()?;
        let folders = folder_transports(
            &self.rclone,
            &self.pool_sync_roots,
            self.policy.native_crypt,
        )?;
        let stores: Vec<&dyn EventStore> =
            transports.iter().map(|s| s as &dyn EventStore).collect();
        let folder_stores: Vec<&dyn EventStore> =
            folders.iter().map(|s| s as &dyn EventStore).collect();
        let (known, known_folders): (BTreeSet<String>, BTreeSet<String>) = {
            let state = self.state.lock().unwrap();
            (
                state.events.keys().cloned().collect(),
                state.folder_records.keys().cloned().collect(),
            )
        };
        let unseen = list_unseen(&stores, &known)?;
        let unseen_folders = list_unseen(&folder_stores, &known_folders)?;
        let incoming_folders = read_folders_best_effort(&folder_stores, &unseen_folders);
        let family = super::metadata_checkpoint_model::Family::v6();
        let dirs = super::metadata_pool::replica_dirs(
            &self.rclone,
            &self.pool_sync_roots,
            self.policy.native_crypt,
            &family,
        )?;
        let replicas: Vec<_> = dirs.iter().map(|d| d.replica()).collect();
        let cache_path = super::metadata_cache::Cache::path(&self.root, family.name);
        let mut cache = super::metadata_cache::Cache::load(&cache_path);
        let mut incoming = BTreeMap::new();
        let covered = super::metadata_checkpoint::pull(
            &family,
            &replicas,
            &mut cache,
            unseen.gate,
            known.is_empty(),
            &mut |_, id, text| {
                if !known.contains(id) {
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
        let tail = read_unseen(&stores, &unseen, |id| {
            incoming.contains_key(id) || covered.contains(id)
        })?;
        incoming.extend(tail);
        let mut state = self.state.lock().unwrap();
        let unacknowledged = covered.iter().any(|id| {
            !state.published.contains(id)
                && (state.events.contains_key(id) || incoming.contains_key(id))
        });
        if incoming.is_empty() && incoming_folders.is_empty() && !unacknowledged {
            drop(state);
            return cache.save(&cache_path);
        }
        let mut next = state.clone();
        next.ingest(incoming)?;
        next.folder_records.extend(incoming_folders);
        // Do not acknowledge downloaded events: normal publication heals missing
        // replicas. Checkpointed events are durable in the checkpoint itself.
        let durable: Vec<_> = covered
            .into_iter()
            .filter(|id| next.events.contains_key(id))
            .collect();
        next.published.extend(durable);
        next.save(&self.root)?;
        *state = next;
        drop(state);
        cache.save(&cache_path)
    }
    /// Publishes unpublished events in waves: an event goes out once all its
    /// parents are published, so interrupted publication stays readable.
    /// Events of one wave go out concurrently ([`PUBLISH_PARALLEL`] at a
    /// time), each to every replica at once; an event counts as published
    /// only after every replica verified it.
    pub(crate) fn publish_pool(&self) -> Result<()> {
        let transports = self.pool_transports()?;
        if transports.is_empty() {
            bail!("invalid metadata publication");
        }
        let pending = self.state.lock().unwrap().unpublished_ordered()?;
        let mut remaining: BTreeMap<String, Event> = pending.into_iter().collect();
        while !remaining.is_empty() {
            let published = self.state.lock().unwrap().published.clone();
            let wave: Vec<(String, Event)> = remaining
                .iter()
                .filter(|(_, e)| e.parents.iter().all(|p| published.contains(p)))
                .map(|(id, e)| (id.clone(), e.clone()))
                .collect();
            if wave.is_empty() {
                bail!("missing or cyclic publication ancestry");
            }
            for chunk in wave.chunks(PUBLISH_PARALLEL) {
                let results: Vec<(String, Result<()>)> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|(id, event)| {
                            let transports = &transports;
                            scope.spawn(move || {
                                (id.clone(), publish_everywhere(transports, id, event))
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join()
                                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                        })
                        .collect()
                });
                let mut first_error = None;
                let mut state = self.state.lock().unwrap();
                let mut next = state.clone();
                for (id, result) in results {
                    match result {
                        Ok(()) => {
                            remaining.remove(&id);
                            next.published.insert(id);
                        }
                        Err(error) => {
                            first_error.get_or_insert(error);
                        }
                    }
                }
                next.save(&self.root)?;
                *state = next;
                drop(state);
                if let Some(error) = first_error {
                    return Err(error);
                }
            }
        }
        let folders = folder_transports(
            &self.rclone,
            &self.pool_sync_roots,
            self.policy.native_crypt,
        )?;
        let mut remaining: Records = {
            let state = self.state.lock().unwrap();
            state
                .folder_records
                .iter()
                .filter(|(id, _)| !state.folders_published.contains(*id))
                .map(|(id, record)| (id.clone(), record.clone()))
                .collect()
        };
        while !remaining.is_empty() {
            let (published, known) = {
                let state = self.state.lock().unwrap();
                (
                    state.folders_published.clone(),
                    state.folder_records.clone(),
                )
            };
            let ready: Vec<_> = remaining
                .iter()
                .filter(|(_, record)| {
                    record
                        .parents
                        .iter()
                        .all(|p| published.contains(p) || !known.contains_key(p))
                })
                .map(|(id, record)| (id.clone(), record.clone()))
                .collect();
            if ready.is_empty() {
                bail!("missing or cyclic folder publication ancestry");
            }
            for (id, record) in ready {
                publish_folder_everywhere(&folders, &id, &record)?;
                let mut state = self.state.lock().unwrap();
                let mut next = state.clone();
                next.folders_published.insert(id.clone());
                next.save(&self.root)?;
                *state = next;
                remaining.remove(&id);
            }
        }
        Ok(())
    }
}

/// Pool-sync state reported in the mount status (GUI conflict list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Status {
    /// v6 metadata roots this drive syncs with.
    pub roots: Vec<String>,
    /// Current concurrent-edit conflicts from [`super::peer_projection::project`].
    pub conflicts: Vec<super::peer_projection::Conflict>,
}
impl super::virtual_drive::VirtualDrive {
    /// Current [`Status`] from the namespace; read by `virtual_drive::run` for
    /// the mount status report.
    pub(crate) fn pool_status(&self) -> Result<Status> {
        let state = self.state.lock().unwrap();
        Ok(Status {
            roots: self.pool_sync_roots.clone(),
            conflicts: super::peer_projection::project(&state.events)?.conflicts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    #[derive(Default)]
    struct Store {
        events: RefCell<BTreeMap<String, Vec<u8>>>,
        fail: Cell<bool>,
    }
    impl EventStore for Store {
        fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
            if self.fail.get() {
                bail!("offline");
            }
            Ok(self
                .events
                .borrow()
                .iter()
                .filter(|(id, _)| !known.contains(*id))
                .map(|(id, b)| (id.clone(), b.clone()))
                .collect())
        }
        fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
            if self.fail.get() {
                bail!("offline");
            }
            self.events.borrow_mut().insert(id.into(), bytes.to_vec());
            Ok(())
        }
    }
    fn event(worker: &str, path: &str, parents: Vec<String>) -> Event {
        Event {
            version: 1,
            worker: worker.into(),
            device: worker.into(),
            path: path.into(),
            parents,
            content: None,
        }
    }
    #[test]
    fn metadata_replication_retries_partial_publication_and_unions_disjoint_pcs() {
        let a = Store::default();
        let b = Store::default();
        let stores: Vec<&dyn EventStore> = vec![&a, &b];
        let first = event("A", "a", vec![]);
        let id = first.id().unwrap();
        b.fail.set(true);
        assert!(replicate(&stores, &id, &first).is_err());
        assert!(a.events.borrow().contains_key(&id));
        assert!(collect(&stores, &BTreeSet::new()).is_err());
        b.fail.set(false);
        replicate(&stores, &id, &first).unwrap();
        let second = event("B", "b", vec![]);
        b.publish(&second.id().unwrap(), &serde_json::to_vec(&second).unwrap())
            .unwrap();
        let incoming = collect(&stores, &BTreeSet::new()).unwrap();
        assert_eq!(incoming.len(), 2);
        for (id, event) in &incoming {
            replicate(&stores, id, event).unwrap();
        }
        assert_eq!(*a.events.borrow(), *b.events.borrow());
        assert!(collect(&stores, &incoming.keys().cloned().collect())
            .unwrap()
            .is_empty());
    }
    #[test]
    fn metadata_rejects_corruption_and_bad_event_identity() {
        let store = Store::default();
        let e = event("A", "file", vec![]);
        store
            .events
            .borrow_mut()
            .insert("0".repeat(64), serde_json::to_vec(&e).unwrap());
        assert!(collect(&[&store], &BTreeSet::new()).is_err());
        assert!(replicate(&[&store], &"0".repeat(64), &e).is_err());
    }
    #[test]
    fn automatic_paths_stable_across_membership_order_and_expansion() {
        let a = roots("my-pool", &["one:explicit".into(), "two:explicit".into()]).unwrap();
        let b = roots("my-pool", &["two:explicit".into(), "one:explicit".into()]).unwrap();
        assert_eq!(a, b);
        let expanded = roots(
            "my-pool",
            &[
                "one:explicit".into(),
                "two:explicit".into(),
                "three:explicit".into(),
            ],
        )
        .unwrap();
        assert!(a.iter().all(|p| expanded.contains(p)));
        assert!(a.iter().all(|p| p.contains("/.rpool-sync/events-v6/")));
        assert_ne!(
            a,
            roots(
                "other-pool",
                &["one:explicit".into(), "two:explicit".into()]
            )
            .unwrap()
        );
    }
    #[test]
    fn folders_round_trip_between_two_offline_namespaces() {
        let replica = Store::default();
        let stores: Vec<&dyn EventStore> = vec![&replica];
        let mut a = super::super::namespace::Namespace::create("a").unwrap();
        let mut b = super::super::namespace::Namespace::create("b").unwrap();
        a.seed_folders().unwrap();
        b.seed_folders().unwrap();
        a.add_folder("empty").unwrap();
        for (id, record) in a.folder_records.iter() {
            replica
                .publish(id, &serde_json::to_vec(record).unwrap())
                .unwrap();
        }
        let unseen = list_unseen(&stores, &BTreeSet::new()).unwrap();
        b.folder_records
            .extend(read_folders(&stores, &unseen).unwrap());
        b.refresh_directories().unwrap();
        assert!(b.directories.contains("empty"));
        b.remove_folder("empty").unwrap();
        for (id, record) in b.folder_records.iter() {
            replica
                .publish(id, &serde_json::to_vec(record).unwrap())
                .unwrap();
        }
        let known = a.folder_records.keys().cloned().collect();
        let unseen = list_unseen(&stores, &known).unwrap();
        a.folder_records
            .extend(read_folders(&stores, &unseen).unwrap());
        a.refresh_directories().unwrap();
        assert!(!a.directories.contains("empty"));
        a.add_folder("empty").unwrap();
        assert!(a.directories.contains("empty"));
    }

    #[test]
    fn unknown_folder_format_does_not_block_file_metadata() {
        let folders = Store::default();
        let good = FolderRecord::new("a", "good", vec![], true).unwrap();
        folders
            .publish(&good.id().unwrap(), &serde_json::to_vec(&good).unwrap())
            .unwrap();
        let mut future = serde_json::to_value(&good).unwrap();
        future["version"] = 2.into();
        let future_bytes = serde_json::to_vec(&future).unwrap();
        let future_id = blake3::hash(&future_bytes).to_hex().to_string();
        folders.publish(&future_id, &future_bytes).unwrap();
        folders
            .events
            .borrow_mut()
            .insert("0".repeat(64), b"not json".to_vec());
        let stores: Vec<&dyn EventStore> = vec![&folders];
        let unseen = list_unseen(&stores, &BTreeSet::new()).unwrap();
        assert!(read_folders(&stores, &unseen).is_err());
        let accepted = read_folders_best_effort(&stores, &unseen);
        assert_eq!(accepted.len(), 1);
        assert!(accepted.contains_key(&good.id().unwrap()));

        let file_store = Store::default();
        let file = event("b", "file.txt", vec![]);
        file_store
            .publish(&file.id().unwrap(), &serde_json::to_vec(&file).unwrap())
            .unwrap();
        assert_eq!(collect(&[&file_store], &BTreeSet::new()).unwrap().len(), 1);
    }
}
