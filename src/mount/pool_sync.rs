//! Automatic immutable metadata replication within the existing encrypted pool.
//! No mutable shared catalog, coordinator, or destructive history collection.
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

pub(crate) trait EventStore {
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>>;
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
    pub entries: BTreeMap<String, (usize, u64)>,
    /// The compaction gate record is listed (deletion may have happened).
    pub gate: bool,
}
pub(crate) fn list_unseen(stores: &[&dyn EventStore], known: &BTreeSet<String>) -> Result<Unseen> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    let gate = super::metadata_checkpoint_model::Family::v6().gate_id();
    let mut result = Unseen {
        entries: BTreeMap::new(),
        gate: false,
    };
    let mut bytes = 0u64;
    for (index, store) in stores.iter().enumerate() {
        // List every configured replica. An outage is an error, never an empty namespace.
        for (id, size) in store
            .unseen(known)
            .context("Pool metadata read failed; keep the workspace")?
        {
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

impl super::virtual_drive::VirtualDrive {
    pub(crate) fn pool_transports(&self) -> Result<Vec<SharedTransport>> {
        self.pool_sync_roots
            .iter()
            .map(|root| {
                SharedTransport::new(&self.rclone, root)
                    .map(|t| t.with_native_crypt(self.policy.native_crypt))
            })
            .collect()
    }
    pub(crate) fn pull_pool(&self) -> Result<()> {
        let transports = self.pool_transports()?;
        let stores: Vec<&dyn EventStore> =
            transports.iter().map(|s| s as &dyn EventStore).collect();
        let known: BTreeSet<String> = self.state.lock().unwrap().events.keys().cloned().collect();
        let unseen = list_unseen(&stores, &known)?;
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
        if incoming.is_empty() && !unacknowledged {
            drop(state);
            return cache.save(&cache_path);
        }
        let mut next = state.clone();
        next.ingest(incoming)?;
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
    pub(crate) fn publish_pool(&self) -> Result<()> {
        let transports = self.pool_transports()?;
        let stores: Vec<&dyn EventStore> =
            transports.iter().map(|s| s as &dyn EventStore).collect();
        let pending = self.state.lock().unwrap().unpublished_ordered()?;
        for (id, event) in pending {
            replicate(&stores, &id, &event)?;
            let mut state = self.state.lock().unwrap();
            let mut next = state.clone();
            next.published.insert(id);
            next.save(&self.root)?;
            *state = next;
        }
        Ok(())
    }
}

/// Read-only access for `rpool pool browse`: the v6 replicas of `pool`, used
/// only through `collect` (listing/reading events).
pub(crate) fn read_stores(
    rclone: &str,
    pool: &str,
    policy: &PoolDefinition,
    epoch: Option<&str>,
) -> Result<Vec<Box<dyn EventStore>>> {
    roots(pool, &policy.remotes)?
        .iter()
        .map(|root| match epoch {
            Some(epoch) => crate::utils::remote_join(root, &format!("epochs/{epoch}")),
            None => root.clone(),
        })
        .collect::<Vec<_>>()
        .iter()
        .map(|root| {
            SharedTransport::new(rclone, root)
                .map(|t| Box::new(t.with_native_crypt(policy.native_crypt)) as Box<dyn EventStore>)
        })
        .collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Status {
    pub roots: Vec<String>,
    pub conflicts: Vec<super::peer_projection::Conflict>,
}
impl super::virtual_drive::VirtualDrive {
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
}
