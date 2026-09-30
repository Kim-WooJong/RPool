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
