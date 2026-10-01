//! A completed synthetic migration for the cleanup tests (no rclone): two
//! switched originals, one orphan copy, one lost archive, and one unrelated
//! archive that keeps the pool large enough for the default guard. Plus an
//! in-memory [`RetireIo`] with a settable clock.
use super::io::RetireIo;
use super::model::{RetireOptions, RetireRecord};
use super::plan::World;
use super::refs::References;
use crate::migration::enumerate::RemoteListing;
use crate::migration::execute::tests_support::{entry, plan, rec};
use crate::migration::model::{Action, Plan, Record, RecordState};
use crate::migration::test_support::manifest;
use crate::prelude::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};

pub(crate) const N1: &str = "migrate-aaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const N2: &str = "migrate-bbbbbbbbbbbbbbbbbbbbbbbb";
pub(crate) const O1: &str = "migrate-cccccccccccccccccccccccc";
pub(crate) const ROOTS: [&str; 3] = ["a:", "b:", "c:"];
pub(crate) const MANIFEST_SIZE: u64 = 100;

fn add_archive(listings: &mut BTreeMap<String, RemoteListing>, m: &Manifest) {
    for shard in &m.shards {
        if let Some(RemoteListing::Listed(files)) = listings.get_mut(&shard.remote) {
            let rel = crate::utils::relative_remote_object(&shard.remote, &shard.object).unwrap();
            files.insert(rel, shard.size);
        }
    }
    for root in ROOTS {
        if let Some(RemoteListing::Listed(files)) = listings.get_mut(root) {
            files.insert(format!("{}/manifest.json", m.archive_id), MANIFEST_SIZE);
        }
    }
}

pub(crate) struct Fixture {
    pub plan: Plan,
    pub records: Vec<Record>,
    pub world: World,
}

pub(crate) fn fixture() -> Fixture {
    let old1 = manifest("old1", 10, 4, 2, 1, &ROOTS);
    let old2 = manifest("old2", 8, 4, 2, 1, &ROOTS);
    let n1 = manifest(N1, 10, 4, 2, 1, &ROOTS);
    let n2 = manifest(N2, 8, 4, 2, 1, &ROOTS);
    let keep = manifest("keep", 400, 4, 2, 1, &ROOTS);
    let mut listings: BTreeMap<String, RemoteListing> = ROOTS
        .iter()
        .map(|r| (r.to_string(), RemoteListing::Listed(BTreeMap::new())))
        .collect();
    for m in [&old1, &old2, &n1, &n2, &keep] {
        add_archive(&mut listings, m);
    }
    if let Some(RemoteListing::Listed(files)) = listings.get_mut("a:") {
        files.insert(format!("{O1}/data/00000000.bin"), 4);
    }
    let fp = |m: &Manifest| crate::manifest::manifest_fingerprint(m).unwrap();
    let mut e1 = entry("old1", Action::Relocate, 10);
    e1.fingerprint = fp(&old1);
    let mut e2 = entry("old2", Action::Reencode, 8);
    e2.fingerprint = fp(&old2);
    let mut p = plan(vec![e1, e2, entry("lost1", Action::Lost, 3)]);
    p.target.remotes = ROOTS.iter().map(|r| r.to_string()).collect();
    let switched = |e: &str, id: &str, state: RecordState, ts: u64| {
        let mut r = rec(e, state, ts, "pc1");
        r.new_archive_id = Some(id.into());
        r.new_manifest = matches!(state, RecordState::Verified | RecordState::Switched)
            .then(|| format!("a:{id}/manifest.json"));
        r
    };
    let records = vec![
        switched("old1", N1, RecordState::Claimed, 1),
        switched("old1", N1, RecordState::Verified, 2),
        switched("old1", N1, RecordState::Switched, 3),
        switched("old2", O1, RecordState::Claimed, 4),
        switched("old2", O1, RecordState::Orphan, 5),
        switched("old2", N2, RecordState::Claimed, 6),
        switched("old2", N2, RecordState::Verified, 7),
        switched("old2", N2, RecordState::Switched, 8),
        rec("lost1", RecordState::Lost, 9, "pc1"),
    ];
    let mut refs = References::default();
    for m in [&n1, &n2, &keep] {
        refs.add_manifest(&format!("manifest of {}", m.archive_id), m);
    }
    let world = World {
        pool_roots: ROOTS.iter().map(|r| r.to_string()).collect(),
        listings,
        originals: BTreeMap::from([
            ("old1".to_string(), Ok(Some(old1))),
            ("old2".to_string(), Ok(Some(old2))),
        ]),
        replacements: BTreeMap::from([(N1.to_string(), Ok(n1)), (N2.to_string(), Ok(n2))]),
        full_checks: BTreeMap::new(),
        refs,
    };
    Fixture {
        plan: p,
        records,
        world,
    }
}

/// In-memory cleanup side effects. Deleting removes the object from the
/// world's listings; `fail_after` makes the n-th next delete fail.
pub(crate) struct FakeIo {
    pub plan: Plan,
    pub records: Vec<Record>,
    pub world: Mutex<World>,
    pub journal: Mutex<Vec<RetireRecord>>,
    pub deleted: Mutex<Vec<String>>,
    pub forgotten: Mutex<Vec<String>>,
    pub clock: AtomicU64,
    ids: AtomicUsize,
    pub fail_after: Mutex<Option<usize>>,
    pub observed: AtomicUsize,
}

impl FakeIo {
    pub(crate) fn new(f: Fixture) -> Self {
        Self {
            plan: f.plan,
            records: f.records,
            world: Mutex::new(f.world),
            journal: Mutex::new(vec![]),
            deleted: Mutex::new(vec![]),
            forgotten: Mutex::new(vec![]),
            clock: AtomicU64::new(1_000),
            ids: AtomicUsize::new(0),
            fail_after: Mutex::new(None),
            observed: AtomicUsize::new(0),
        }
    }
    pub(crate) fn kinds(&self) -> Vec<(super::model::RetireKind, String)> {
        self.journal
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.kind, r.item.clone()))
            .collect()
    }
}

impl RetireIo for FakeIo {
    fn plan(&self) -> Result<Plan> {
        Ok(self.plan.clone())
    }
    fn records(&self) -> Result<Vec<Record>> {
        Ok(self.records.clone())
    }
    fn retire_records(&self) -> Result<Vec<RetireRecord>> {
        Ok(self.journal.lock().unwrap().clone())
    }
    fn append(&self, record: &RetireRecord) -> Result<()> {
        self.journal.lock().unwrap().push(record.clone());
        Ok(())
    }
    fn observe(&self, _: &Plan, _: &[Record], _: &RetireOptions) -> Result<World> {
        self.observed.fetch_add(1, SeqCst);
        Ok(self.world.lock().unwrap().clone())
    }
    fn delete(&self, address: &str) -> Result<()> {
        let mut fail = self.fail_after.lock().unwrap();
        if let Some(n) = fail.as_mut() {
            if *n == 0 {
                *fail = None;
                bail!("provider error while deleting");
            }
            *n -= 1;
        }
        let (remote, path) = address.split_once(':').unwrap();
        let root = format!("{remote}:");
        if let Some(RemoteListing::Listed(files)) =
            self.world.lock().unwrap().listings.get_mut(&root)
        {
            files.remove(path);
        }
        self.deleted.lock().unwrap().push(address.into());
        Ok(())
    }
    fn forget(&self, archive_id: &str) -> Result<()> {
        self.forgotten.lock().unwrap().push(archive_id.into());
        Ok(())
    }
    fn now(&self) -> u64 {
        self.clock.load(SeqCst)
    }
    fn new_id(&self) -> Result<String> {
        Ok(format!("id{}", self.ids.fetch_add(1, SeqCst)))
    }
    fn pc_id(&self) -> String {
        "pc-test".into()
    }
    fn say(&self, _: &str) {}
}
