//! Automatic immutable metadata replication within the existing encrypted pool.
//! No mutable shared catalog, coordinator, or destructive history collection.
use super::namespace::durable_json;
use super::shared_model::Event;
use super::shared_transport::SharedTransport;
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Desired previous revisions per file, reserved for future safe peer GC.
    #[serde(default)]
    pub history_limit: usize,
}
impl Config {
    pub(crate) fn load(root: &Path, requested: Option<usize>) -> Result<Self> {
        let path = root.join("pool-sync-config.json");
        let mut value: Self = if path.exists() {
            crate::utils::read_json(&path)?
        } else {
            Self::default()
        };
        if let Some(limit) = requested {
            value.history_limit = limit;
        }
        if value.history_limit > 10_000 {
            bail!("history_limit must be between 0 and 10000");
        }
        durable_json(&path, &value)?;
        Ok(value)
    }
}

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

pub(crate) trait EventStore {
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>>;
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()>;
}
impl EventStore for SharedTransport {
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        self.list_missing(known)
    }
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        SharedTransport::publish(self, id, bytes)
    }
}
pub(crate) fn collect(
    stores: &[&dyn EventStore],
    known: &BTreeSet<String>,
) -> Result<BTreeMap<String, Event>> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    let mut events = BTreeMap::new();
    let mut total = 0usize;
    for store in stores {
        // Read every configured replica. An outage is an error, never an empty namespace.
        for (id, bytes) in store.missing(known).context("Pool metadata read failed; keep the workspace. A fresh bootstrap is limited to 10,000 unseen events / 64 MiB per replica; peer compaction is not yet implemented")? {
            if known.contains(&id) { continue; }
            let event: Event = serde_json::from_slice(&bytes)?;
            event.validate()?;
            if event.id()? != id { bail!("pool metadata event identity mismatch"); }
            if !events.contains_key(&id) {
                total = total.checked_add(bytes.len()).context("metadata size overflow")?;
                if total > 64 * 1024 * 1024 || events.len() >= 10_000 { bail!("pool metadata bootstrap limit exceeded; preserve workspace"); }
            }
            events.insert(id, event);
        }
    }
    Ok(events)
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
        let known = self.state.lock().unwrap().events.keys().cloned().collect();
        let incoming = collect(&stores, &known)?;
        if incoming.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        next.ingest(incoming)?;
        // Do not acknowledge downloaded events: normal publication heals missing replicas.
        next.save(&self.root)?;
        *state = next;
        Ok(())
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
) -> Result<Vec<Box<dyn EventStore>>> {
    roots(pool, &policy.remotes)?
        .iter()
        .map(|root| {
            SharedTransport::new(rclone, root)
                .map(|t| Box::new(t.with_native_crypt(policy.native_crypt)) as Box<dyn EventStore>)
        })
        .collect()
}
/// Read-only v7 listing (path -> plaintext size, conflicts) through a throwaway
/// pool-sync workspace in a temp dir, exactly as a fresh v7 mount would see it.
/// `open` touches only the local workspace; the pull only lists/reads records.
/// `None` when the pool has no v7 records.
#[allow(clippy::type_complexity)]
pub(crate) fn browse_v7(
    rclone: &str,
    pool: &str,
) -> Result<Option<(BTreeMap<String, u64>, Vec<super::peer_projection::Conflict>)>> {
    let temp = tempfile::tempdir()?;
    let mut drive = super::virtual_drive::VirtualDrive::open(
        rclone,
        pool,
        &temp.path().join("workspace"),
        "rpool-browse",
        None,
        1 << 20,
        false,
        true,
        true,
    )?;
    if !drive.pull_snapshots_adopting_policy()? {
        return Ok(None);
    }
    let files = drive
        .view()?
        .into_iter()
        .map(|(path, revision)| (path, revision.size()))
        .collect();
    let conflicts = drive.pool_status()?.conflicts;
    Ok(Some((files, conflicts)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Status {
    pub roots: Vec<String>,
    pub desired_history_limit: usize,
    pub history_deletion_enabled: bool,
    pub conflicts: Vec<super::peer_projection::Conflict>,
}
impl super::virtual_drive::VirtualDrive {
    pub(crate) fn pool_status(&self) -> Result<Status> {
        if self.peer_retention {
            return Ok(Status {
                roots: self.pool_sync_roots.clone(),
                desired_history_limit: self.pool_history_limit,
                history_deletion_enabled: true,
                conflicts: self.snapshot_conflicts()?,
            });
        }
        let state = self.state.lock().unwrap();
        Ok(Status {
            roots: self.pool_sync_roots.clone(),
            desired_history_limit: self.pool_history_limit,
            history_deletion_enabled: false,
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
    fn future_history_config_persists_ten_and_rejects_invalid_without_overwrite() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(root.path(), None).unwrap().history_limit, 0);
        assert_eq!(
            Config::load(root.path(), Some(10)).unwrap().history_limit,
            10
        );
        assert_eq!(Config::load(root.path(), None).unwrap().history_limit, 10);
        assert!(Config::load(root.path(), Some(10001)).is_err());
        assert_eq!(Config::load(root.path(), None).unwrap().history_limit, 10);
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
