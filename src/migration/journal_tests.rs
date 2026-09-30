use super::*;
use crate::migration::model::{Counts, RecordState};
use std::sync::atomic::{AtomicBool, Ordering};

/// In-memory replica; `down` makes every call fail like an unreachable remote.
#[derive(Default)]
struct Fake {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    down: AtomicBool,
}
impl Fake {
    fn check(&self) -> Result<()> {
        if self.down.load(Ordering::SeqCst) {
            bail!("provider unreachable");
        }
        Ok(())
    }
    fn put_raw(&self, key: &str, bytes: &[u8]) {
        self.objects
            .lock()
            .unwrap()
            .insert(key.into(), bytes.into());
    }
    fn len(&self) -> usize {
        self.objects.lock().unwrap().len()
    }
}
impl JournalStore for Fake {
    fn label(&self) -> String {
        "fake:".into()
    }
    fn list_migrations(&self) -> Result<Vec<String>> {
        self.check()?;
        let objects = self.objects.lock().unwrap();
        let ids: BTreeSet<String> = objects
            .keys()
            .filter_map(|k| k.split_once('/').map(|(id, _)| id.to_owned()))
            .collect();
        Ok(ids.into_iter().collect())
    }
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        self.check()?;
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(&format!("{migration}/{rel}"))
            .cloned())
    }
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        self.check()?;
        let mut objects = self.objects.lock().unwrap();
        let key = format!("{migration}/{rel}");
        if let Some(existing) = objects.get(&key) {
            return Ok(Some(existing.clone()));
        }
        objects.insert(key, bytes.into());
        Ok(None)
    }
    fn list_records(&self, migration: &str) -> Result<Vec<String>> {
        self.check()?;
        let prefix = format!("{migration}/records/");
        Ok(self
            .objects
            .lock()
            .unwrap()
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix)?.strip_suffix(".json"))
            .map(str::to_owned)
            .collect())
    }
}

fn policy() -> PoolDefinition {
    serde_json::from_value(serde_json::json!({
        "remotes": ["c1:", "c2:"], "shard_mib": 1, "workers": 1, "retries": 1,
        "placement": "round-robin", "data_shards": 1, "parity_shards": 1
    }))
    .unwrap()
}

fn plan(id: &str, created: u64) -> Plan {
    Plan {
        version: 1,
        migration_id: id.into(),
        pool: "p".into(),
        created_unix: created,
        created_by: "pc-a".into(),
        target: policy(),
        entries: vec![],
        counts: Counts::default(),
        download_bytes: 1,
        upload_bytes: 2,
        new_storage_bytes: 3,
        estimated_seconds: Some((1.5, 2.25)),
        download_mib_s: Some(0.1),
        upload_mib_s: None,
        quota_ok: Some(true),
        notes: vec![],
    }
}

fn record(entry: &str, state: RecordState, ts: u64) -> Record {
    Record {
        entry: entry.into(),
        state,
        attempt_id: format!("att-{ts}"),
        pc_id: "pc".into(),
        ts_unix: ts,
        new_archive_id: None,
        new_manifest: None,
        losses: vec![],
        detail: None,
    }
}

type Parts = (Arc<Fake>, Arc<Fake>, tempfile::TempDir);

fn parts() -> Parts {
    (
        Arc::new(Fake::default()),
        Arc::new(Fake::default()),
        tempfile::tempdir().unwrap(),
    )
}

fn journal(id: &str, (a, b, dir): &Parts) -> Journal {
    let cloud: Vec<Arc<dyn JournalStore>> = vec![a.clone(), b.clone()];
    let cache: Arc<dyn JournalStore> = Arc::new(LocalStore::new(dir.path().join("p")));
    Journal::with_stores("p", id, cloud, Some(cache)).unwrap()
}

#[test]
fn publish_plan_is_idempotent_and_conflicts_are_refused() {
    let parts = parts();
    let j = journal("m1", &parts);
    assert!(j.load_plan().unwrap().is_none());
    j.publish_plan(&plan("m1", 10)).unwrap();
    j.publish_plan(&plan("m1", 10)).unwrap();
    assert_eq!(j.load_plan().unwrap().unwrap().created_unix, 10);
    let error = j.publish_plan(&plan("m1", 11)).unwrap_err().to_string();
    assert!(error.contains("different plan"), "{error}");
    // Nothing was overwritten.
    assert_eq!(j.load_plan().unwrap().unwrap().created_unix, 10);
    // Wrong id or pool is refused.
    assert!(j.publish_plan(&plan("m2", 10)).is_err());
    assert!(parts.2.path().join("p/m1/plan.json").exists());
}

#[test]
fn plan_conflict_on_one_replica_is_refused_before_writing() {
    let parts = parts();
    parts.1.put_raw(
        "m1/plan.json",
        &serde_json::to_vec(&plan("m1", 99)).unwrap(),
    );
    let j = journal("m1", &parts);
    assert!(j.publish_plan(&plan("m1", 10)).is_err());
    assert_eq!(parts.0.len(), 0, "nothing written to the other replica");
    let error = {
        parts.0.put_raw(
            "m1/plan.json",
            &serde_json::to_vec(&plan("m1", 10)).unwrap(),
        );
        j.load_plan().unwrap_err().to_string()
    };
    assert!(error.contains("disagree"), "{error}");
}

#[test]
fn plan_publish_needs_one_replica_and_heals_later() {
    let parts = parts();
    parts.1.down.store(true, Ordering::SeqCst);
    let j = journal("m1", &parts);
    j.publish_plan(&plan("m1", 10)).unwrap();
    assert_eq!(parts.1.len(), 0);
    parts.1.down.store(false, Ordering::SeqCst);
    j.publish_plan(&plan("m1", 10)).unwrap();
    assert_eq!(parts.1.len(), 1);
    parts.0.down.store(true, Ordering::SeqCst);
    parts.1.down.store(true, Ordering::SeqCst);
    assert!(j.publish_plan(&plan("m1", 10)).is_err());
}

#[test]
fn append_and_union_across_replicas_with_one_failing() {
    let parts = parts();
    let j = journal("m1", &parts);
    j.publish_plan(&plan("m1", 10)).unwrap();
    j.append(&record("a", RecordState::Claimed, 1)).unwrap();
    parts.1.down.store(true, Ordering::SeqCst);
    j.append(&record("a", RecordState::Verified, 2)).unwrap();
    parts.1.down.store(false, Ordering::SeqCst);
    parts.0.down.store(true, Ordering::SeqCst);
    j.append(&record("b", RecordState::Switched, 3)).unwrap();
    parts.0.down.store(false, Ordering::SeqCst);
    // Duplicate append is harmless.
    j.append(&record("b", RecordState::Switched, 3)).unwrap();
    // A second PC with an empty cache sees the union of both replicas.
    let other = (
        parts.0.clone(),
        parts.1.clone(),
        tempfile::tempdir().unwrap(),
    );
    let mut got = journal("m1", &other).records().unwrap();
    got.sort_by_key(|r| r.ts_unix);
    assert_eq!(
        got.iter().map(|r| r.ts_unix).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    parts.0.down.store(true, Ordering::SeqCst);
    parts.1.down.store(true, Ordering::SeqCst);
    assert!(j.append(&record("c", RecordState::Claimed, 4)).is_err());
}

#[test]
fn bad_records_are_skipped_and_duplicates_collapse() {
    let parts = parts();
    let j = journal("m1", &parts);
    j.append(&record("a", RecordState::Claimed, 1)).unwrap();
    let bad = b"not json";
    let bad_id = blake3::hash(bad).to_hex().to_string();
    parts.0.put_raw(&format!("m1/records/{bad_id}.json"), bad);
    let wrong_name = "0".repeat(64);
    let valid = serde_json::to_vec(&record("x", RecordState::Lost, 7)).unwrap();
    parts
        .1
        .put_raw(&format!("m1/records/{wrong_name}.json"), &valid);
    let fresh = (
        parts.0.clone(),
        parts.1.clone(),
        tempfile::tempdir().unwrap(),
    );
    let got = journal("m1", &fresh).records().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].entry, "a");
}

#[test]
fn local_cache_serves_status_offline() {
    let parts = parts();
    let j = journal("m1", &parts);
    j.publish_plan(&plan("m1", 10)).unwrap();
    j.append(&record("a", RecordState::Switched, 1)).unwrap();
    // A record written by another PC is cached here on the next read.
    let other = (
        parts.0.clone(),
        parts.1.clone(),
        tempfile::tempdir().unwrap(),
    );
    journal("m1", &other)
        .append(&record("b", RecordState::Verified, 2))
        .unwrap();
    assert_eq!(j.records().unwrap().len(), 2);
    parts.0.down.store(true, Ordering::SeqCst);
    parts.1.down.store(true, Ordering::SeqCst);
    assert_eq!(j.records().unwrap().len(), 2);
    assert_eq!(j.load_plan().unwrap().unwrap().created_unix, 10);
    let cloud: Vec<Arc<dyn JournalStore>> = vec![parts.0.clone(), parts.1.clone()];
    let cache: Arc<dyn JournalStore> = Arc::new(LocalStore::new(parts.2.path().join("p")));
    assert_eq!(discover_in("p", cloud, Some(cache)).unwrap(), vec!["m1"]);
    // Never seen here and nothing reachable: an error, not an empty journal.
    let unseen = journal("m9", &parts);
    assert!(unseen.records().is_err());
    assert!(unseen.load_plan().is_err());
}

#[test]
fn discover_is_newest_first_and_skips_unreadable_plans() {
    let parts = parts();
    for (id, created) in [("old", 5), ("new", 50), ("mid", 20)] {
        journal(id, &parts)
            .publish_plan(&plan(id, created))
            .unwrap();
    }
    parts.1.put_raw("broken/plan.json", b"{");
    parts.0.put_raw("noplan/records/x.json", b"{}");
    let cloud: Vec<Arc<dyn JournalStore>> = vec![parts.0.clone(), parts.1.clone()];
    // Fresh cache: ids come from the cloud only.
    let dir = tempfile::tempdir().unwrap();
    let cache: Arc<dyn JournalStore> = Arc::new(LocalStore::new(dir.path().join("p")));
    assert_eq!(
        discover_in("p", cloud.clone(), Some(cache)).unwrap(),
        vec!["new", "mid", "old"]
    );
    parts.0.down.store(true, Ordering::SeqCst);
    parts.1.down.store(true, Ordering::SeqCst);
    assert!(discover_in("p", cloud, None).is_err());
}

#[test]
fn roots_are_siblings_of_pool_sync_events() {
    let policy = policy();
    let ours = roots("p", &policy).unwrap();
    let sync = crate::mount::pool_sync::roots("p", &policy.remotes).unwrap();
    assert_eq!(ours.len(), 2);
    let mapped: Vec<String> = sync
        .iter()
        .map(|r| r.replace(".rpool-sync/events-v6/", ".rpool-sync/migrations-v1/"))
        .collect();
    assert_eq!(ours, mapped);
    assert!(validate_migration_id("../x").is_err());
    assert!(validate_migration_id(".hidden").is_err());
    assert!(validate_migration_id("m-2026_09.30").is_ok());
    assert!(CloudStore::new("rclone", "c1:a/../b", false).is_err());
}

/// Docker e2e (see scripts/linux-docker/migrate-journal-e2e.sh): real rclone
/// crypt remotes, two PCs (two caches), native crypt per `RPOOL_JOURNAL_NATIVE`.
#[test]
#[ignore]
fn e2e_two_pcs_over_crypt_remotes() {
    let rclone = std::env::var("RPOOL_JOURNAL_RCLONE").unwrap_or_else(|_| "rclone".into());
    let native = std::env::var("RPOOL_JOURNAL_NATIVE").is_ok_and(|v| v == "1");
    let cache_b = PathBuf::from(std::env::var("RPOOL_JOURNAL_CACHE_B").unwrap());
    let mut policy = crate::pool::load_pool_store().unwrap().pools["jpool"].clone();
    assert_eq!(policy.native_crypt, native);
    // PC A: the real entry points (saved pool, config-dir cache).
    let a = Journal::open(&rclone, "jpool", "mig-secret-name").unwrap();
    let mut p = plan("mig-secret-name", 1_000);
    p.pool = "jpool".into();
    p.target = policy.clone();
    p.notes = vec!["archive-secret-id-4242".into()];
    a.publish_plan(&p).unwrap();
    a.publish_plan(&p).unwrap();
    a.append(&record("archive-secret-id-4242", RecordState::Claimed, 1))
        .unwrap();
    // PC B: same remotes, its own cache.
    policy.remotes.sort();
    let cloud = cloud_stores(&rclone, "jpool", &policy).unwrap();
    let cache: Arc<dyn JournalStore> = Arc::new(LocalStore::new(cache_b.join("jpool")));
    let b = Journal::with_stores(
        "jpool",
        "mig-secret-name",
        cloud.clone(),
        Some(cache.clone()),
    )
    .unwrap();
    assert_eq!(b.load_plan().unwrap().unwrap().created_unix, 1_000);
    b.append(&record("archive-secret-id-4242", RecordState::Verified, 2))
        .unwrap();
    let mut conflicting = p.clone();
    conflicting.created_unix = 2_000;
    assert!(b.publish_plan(&conflicting).is_err());
    assert_eq!(a.records().unwrap().len(), 2);
    assert_eq!(b.records().unwrap().len(), 2);
    assert_eq!(
        discover_in("jpool", cloud, Some(cache)).unwrap(),
        vec!["mig-secret-name"]
    );
    assert_eq!(discover(&rclone, "jpool").unwrap(), vec!["mig-secret-name"]);
    eprintln!("E2E JOURNAL OK native={}", u8::from(native));
}

/// Named replica whose every call takes `delay` (a slow provider).
struct Slow {
    name: &'static str,
    delay: std::time::Duration,
    inner: Fake,
    reads: std::sync::atomic::AtomicUsize,
}
impl Slow {
    fn new(name: &'static str, delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            name,
            delay: std::time::Duration::from_millis(delay_ms),
            inner: Fake::default(),
            reads: Default::default(),
        })
    }
    fn nap(&self) {
        std::thread::sleep(self.delay);
    }
}
impl JournalStore for Slow {
    fn label(&self) -> String {
        format!("{}:", self.name)
    }
    fn list_migrations(&self) -> Result<Vec<String>> {
        self.nap();
        self.inner.list_migrations()
    }
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        self.nap();
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(migration, rel)
    }
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        self.nap();
        self.inner.create(migration, rel, bytes)
    }
    fn list_records(&self, migration: &str) -> Result<Vec<String>> {
        self.nap();
        self.inner.list_records(migration)
    }
}

fn slow_journal(stores: &[Arc<Slow>], cache: Option<&Path>) -> Journal {
    let cloud = stores
        .iter()
        .map(|s| Arc::clone(s) as Arc<dyn JournalStore>)
        .collect();
    let cache = cache.map(|d| Arc::new(LocalStore::new(d.join("p"))) as Arc<dyn JournalStore>);
    Journal::with_stores("p", "m1", cloud, cache).unwrap()
}

#[test]
fn append_writes_every_replica_concurrently() {
    let stores: Vec<_> = ["s0", "s1", "s2", "s3", "s4"]
        .into_iter()
        .map(|n| Slow::new(n, 200))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let j = slow_journal(&stores, Some(dir.path()));
    let started = std::time::Instant::now();
    j.append(&record("a", RecordState::Claimed, 1)).unwrap();
    let elapsed = started.elapsed();
    // Sequential would take 5 x 200 ms.
    assert!(
        elapsed < std::time::Duration::from_millis(600),
        "{elapsed:?}"
    );
    assert!(stores.iter().all(|s| s.inner.len() == 1));
    let started = std::time::Instant::now();
    j.publish_plan(&plan("m1", 10)).unwrap();
    // One concurrent read round plus one concurrent create round.
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "{elapsed:?}"
    );
    assert!(stores.iter().all(|s| s.inner.len() == 2));
}

#[test]
fn concurrent_append_keeps_failure_semantics_in_store_order() {
    let stores: Vec<_> = ["s0", "s1", "s2"]
        .into_iter()
        .map(|n| Slow::new(n, 50))
        .collect();
    let j = slow_journal(&stores, None);
    stores[1].inner.down.store(true, Ordering::SeqCst);
    j.append(&record("a", RecordState::Claimed, 1)).unwrap();
    assert_eq!(
        stores.iter().map(|s| s.inner.len()).collect::<Vec<_>>(),
        vec![1, 0, 1]
    );
    // A differing object at the content address is a failure, not a success.
    let bytes = serde_json::to_vec(&record("b", RecordState::Claimed, 2)).unwrap();
    let rel = record_path(&blake3::hash(&bytes).to_hex());
    stores[2].inner.put_raw(&format!("m1/{rel}"), b"other");
    stores[0].inner.down.store(true, Ordering::SeqCst);
    let error = j
        .append(&record("b", RecordState::Claimed, 2))
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "migration record could not be stored on any pool remote: \
         s0:: provider unreachable; s1:: provider unreachable; \
         s2:: existing record differs from its content address"
    );
}

#[test]
fn records_merge_concurrent_replicas_like_sequential_reads() {
    let stores: Vec<_> = ["s0", "s1", "s2"]
        .into_iter()
        .map(|n| Slow::new(n, 100))
        .collect();
    let put = |store: &Slow, r: &Record| {
        let bytes = serde_json::to_vec(r).unwrap();
        let rel = record_path(&blake3::hash(&bytes).to_hex());
        store.inner.put_raw(&format!("m1/{rel}"), &bytes);
        rel
    };
    let everywhere: Vec<_> = (1..=6)
        .map(|ts| record("all", RecordState::Claimed, ts))
        .collect();
    for r in &everywhere {
        for store in &stores {
            put(store, r);
        }
    }
    put(&stores[2], &record("only-s2", RecordState::Verified, 20));
    // s0 holds a corrupt copy of a record that s1 holds intact: the fallback
    // read on s1 must still find it.
    let good = record("healed", RecordState::Switched, 30);
    let rel = put(&stores[1], &good);
    stores[0].inner.put_raw(&format!("m1/{rel}"), b"corrupt");
    // An unreachable replica only adds a warning.
    let down = Slow::new("s3", 100);
    put(&down, &record("lost-with-s3", RecordState::Lost, 40));
    down.inner.down.store(true, Ordering::SeqCst);
    let mut all = stores.clone();
    all.push(down);

    let dir = tempfile::tempdir().unwrap();
    let j = slow_journal(&all, Some(dir.path()));
    let started = std::time::Instant::now();
    let mut got: Vec<u64> = j.records().unwrap().iter().map(|r| r.ts_unix).collect();
    let elapsed = started.elapsed();
    got.sort_unstable();
    assert_eq!(got, vec![1, 2, 3, 4, 5, 6, 20, 30]);
    // 8 distinct records: each downloaded once (+1 retry of the corrupt copy),
    // spread over the replicas instead of all from the first one.
    let reads: usize = stores.iter().map(|s| s.reads.load(Ordering::SeqCst)).sum();
    assert_eq!(reads, 9);
    // Sequential: 4 listings + 8 reads = 1.2 s at least.
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "{elapsed:?}"
    );
    // Everything is now cached: a second read downloads nothing.
    let before = reads;
    assert_eq!(j.records().unwrap().len(), 8);
    let after: usize = stores.iter().map(|s| s.reads.load(Ordering::SeqCst)).sum();
    assert_eq!(after, before);
}

#[test]
fn appends_from_several_threads_share_one_journal() {
    let parts = parts();
    let j = journal("m1", &parts);
    std::thread::scope(|scope| {
        for ts in 1..=8 {
            let j = &j;
            scope.spawn(move || j.append(&record("a", RecordState::Claimed, ts)).unwrap());
        }
    });
    assert_eq!(parts.0.len(), 8);
    assert_eq!(parts.1.len(), 8);
    assert_eq!(j.records().unwrap().len(), 8);
}
