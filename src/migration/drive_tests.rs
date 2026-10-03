//! Drive migration end to end over fakes: plan, interrupted and resumed run
//! (real state machine and cloud journal over local stores), adoption with
//! catch-up, what a PC without a workspace and a PC on the old generation
//! see afterwards.
use super::*;
use crate::migration::drive_generations::{effective, fence, fresh_workspace, Fence};
use crate::migration::drive_journal::{known_in, publish_plan};
use crate::migration::drive_model::DriveEntry;
use crate::migration::drive_plan::{classify, extend_listings, plan_drive};
use crate::migration::drive_run::{as_entry, by_key, new_archive_id};
use crate::migration::drive_status::summarize;
use crate::migration::drive_test_support::{drive_file, FakeDrive};
use crate::migration::execute::{run_core, Effects, Replacement};
use crate::migration::journal::{JournalStore, LocalStore};
use crate::migration::model::{Entry, Record, RecordState};
use crate::migration::plan::{list_all, plan_listed, PlanOptions};
use crate::migration::test_support::{manifest, policy, FakeCloud};
use crate::mount::drive_generation_write::Sink;
use crate::pool::browse_generations::Generation;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

const MIB: u64 = 1048576;
const TARGET: [&str; 3] = ["a:x", "b:x", "c:x"];

struct World {
    dir: tempfile::TempDir,
    cloud: FakeCloud,
    drive: FakeDrive,
    plan: Plan,
    drive_plan: DrivePlan,
    clock: AtomicU64,
    /// Replacement manifests by location.
    manifests: Mutex<BTreeMap<String, Manifest>>,
    builds_left: AtomicUsize,
}

fn payload(n: char, remotes: &[&str]) -> Manifest {
    let id = format!("virtual-{}", n.to_string().repeat(64));
    manifest(&id, 2 * MIB, MIB, 2, 1, remotes)
}

/// Pool a,b,c,d loses d. Files: kept (a,b,c), moved1 / moved2 (b,c,d),
/// gone (two shards on d).
fn world() -> World {
    let mut cloud = FakeCloud::default();
    let manifests = vec![
        payload('1', &["a:x", "b:x", "c:x"]),
        payload('2', &["b:x", "c:x", "d:x"]),
        payload('3', &["b:x", "c:x", "d:x"]),
        payload('4', &["d:x", "d:x", "c:x"]),
    ];
    for m in &manifests {
        cloud.store(m, &[]);
    }
    cloud.wipe("d:x");
    let names = ["docs/kept.txt", "moved1.bin", "moved2.bin", "gone.bin"];
    let files = manifests
        .into_iter()
        .zip(names)
        .map(|(m, name)| drive_file(name, &format!("rev1-{name}"), m))
        .collect();
    world_with(cloud, files)
}

/// The same pool with `files` (planned and published like [`world`]).
fn world_with(cloud: FakeCloud, files: Vec<DriveFile>) -> World {
    let drive = FakeDrive::new(files);
    let options = PlanOptions::default();
    let (mut plan, mut listed) = plan_listed(
        &cloud,
        "p",
        policy(&TARGET, MIB, 2, 1),
        &InventoryStore::default(),
        &options,
        &BTreeSet::new(),
    )
    .unwrap();
    let drive_plan = plan_drive(&cloud, &drive, &mut plan, &mut listed, &options)
        .unwrap()
        .unwrap();
    let world = World {
        dir: tempfile::tempdir().unwrap(),
        cloud,
        drive,
        plan,
        drive_plan,
        clock: AtomicU64::new(1_000),
        manifests: Mutex::new(BTreeMap::new()),
        builds_left: AtomicUsize::new(usize::MAX),
    };
    let journal = world.journal("cache-a");
    publish_plan(&journal, &world.drive_plan).unwrap();
    journal.publish_plan(&world.plan).unwrap();
    world
}

impl World {
    fn stores(&self) -> Vec<Arc<dyn JournalStore>> {
        ["r1", "r2"]
            .iter()
            .map(|r| Arc::new(LocalStore::new(self.dir.path().join(r))) as Arc<dyn JournalStore>)
            .collect()
    }
    fn cache(&self, name: &str) -> Arc<dyn JournalStore> {
        Arc::new(LocalStore::new(self.dir.path().join(name)))
    }
    /// The journal as one PC (its own local cache) sees it.
    fn journal(&self, cache: &str) -> Journal {
        Journal::with_stores(
            "p",
            &self.plan.migration_id,
            self.stores(),
            Some(self.cache(cache)),
        )
        .unwrap()
    }
    fn now(&self) -> u64 {
        self.clock.load(SeqCst)
    }
    fn files(&self) -> BTreeMap<String, DriveFile> {
        by_key(crate::migration::drive_model::units(
            &self.drive.files.lock().unwrap(),
        ))
    }
    /// The run's drive part for `pc`: freeze, then the planned entries still
    /// current, through the real state machine.
    fn run(&self, pc: &str, cache: &str) -> RunSummary {
        let journal = self.journal(cache);
        super::super::drive_journal::freeze(
            &journal,
            &DriveFreeze {
                version: 1,
                migration_id: self.plan.migration_id.clone(),
                source: self.drive_plan.source.clone(),
                epoch: self.drive_plan.epoch.clone(),
                pc_id: pc.into(),
                ts_unix: self.now(),
            },
        )
        .unwrap();
        let files = self.files();
        let entries: Vec<Entry> = self
            .drive_plan
            .entries
            .iter()
            .filter(|e| files.contains_key(&e.key))
            .map(as_entry)
            .collect();
        self.run_entries(&journal, entries, files, pc)
    }
    fn run_entries(
        &self,
        journal: &Journal,
        entries: Vec<Entry>,
        files: BTreeMap<String, DriveFile>,
        pc: &str,
    ) -> RunSummary {
        let effects = FakeEffects {
            world: self,
            journal,
            files,
        };
        let work = Plan {
            entries,
            ..self.plan.clone()
        };
        run_core(&work, &journal.records().unwrap(), pc, &effects, Some(1)).unwrap()
    }
    fn adopt(&self, cache: &str, accept_lost: bool) -> Result<DriveAdoption> {
        let journal = self.journal(cache);
        let io = FakeIo {
            world: self,
            journal: &journal,
        };
        adopt_core(
            &journal,
            &self.plan,
            &self.drive_plan,
            &io,
            accept_lost,
            cache,
        )
    }
    fn new_generation(&self) -> GenerationRef {
        GenerationRef {
            epoch: Some(self.drive_plan.epoch.clone()),
        }
    }
}

struct FakeEffects<'a> {
    world: &'a World,
    journal: &'a Journal,
    files: BTreeMap<String, DriveFile>,
}

impl Effects for FakeEffects<'_> {
    fn append(&self, record: &Record) -> Result<()> {
        self.journal.append(record)
    }
    fn source_fingerprint(&self, entry: &Entry) -> Result<String> {
        crate::manifest::manifest_fingerprint(&self.files[&entry.archive_id].manifest)
    }
    fn build(&self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
        let left = self.world.builds_left.load(SeqCst);
        if left != usize::MAX {
            self.world.builds_left.store(left - 1, SeqCst);
        }
        let file = &self.files[&entry.archive_id];
        let built = manifest(new_archive_id, file.size, MIB, 2, 1, &TARGET);
        let location = format!("a:x/{new_archive_id}/manifest.json");
        self.world
            .manifests
            .lock()
            .unwrap()
            .insert(location.clone(), built);
        Ok(Replacement {
            new_archive_id: new_archive_id.into(),
            new_manifest: location,
        })
    }
    fn reverify(&self, _: &Entry, _: &str, location: &str) -> Result<()> {
        match self.world.manifests.lock().unwrap().contains_key(location) {
            true => Ok(()),
            false => bail!("missing"),
        }
    }
    fn switch(&self, _: &Entry, _: &Replacement) -> Result<()> {
        Ok(())
    }
    fn stop_requested(&self) -> bool {
        self.world.builds_left.load(SeqCst) == 0
    }
    fn now(&self) -> u64 {
        self.world.now()
    }
    fn new_archive_id(&self) -> Result<String> {
        new_archive_id()
    }
    fn say(&self, _: &str) {}
}

struct FakeIo<'a> {
    world: &'a World,
    journal: &'a Journal,
}

impl AdoptIo for FakeIo<'_> {
    fn source(&self) -> &dyn DriveSource {
        &self.world.drive
    }
    fn sink(&self, epoch: &str) -> Result<Box<dyn Sink + '_>> {
        Ok(Box::new(self.world.drive.sink(epoch)))
    }
    fn load_manifest(&self, location: &str) -> Result<Manifest> {
        self.world
            .manifests
            .lock()
            .unwrap()
            .get(location)
            .cloned()
            .context("no such manifest")
    }
    fn catch_up(&self, files: &[DriveFile]) -> Result<(RunSummary, BTreeSet<String>)> {
        let w = self.world;
        let target = &w.plan.target;
        let set: BTreeSet<String> = target.remotes.iter().cloned().collect();
        let mut listings = list_all(&w.cloud, &set, None);
        extend_listings(&w.cloud, &mut listings, files);
        let classified = classify(
            &w.cloud,
            target,
            &set,
            &listings,
            &PlanOptions::default(),
            1,
            files,
        )?;
        let kept = classified
            .iter()
            .filter(|(e, _)| e.action == Action::Unaffected)
            .map(|(e, _)| e.key.clone())
            .collect();
        let entries = classified
            .iter()
            .filter(|(e, _)| e.action != Action::Unaffected)
            .map(|(e, _)| as_entry(e))
            .collect();
        let summary = w.run_entries(self.journal, entries, by_key(files.to_vec()), "adopter");
        Ok((summary, kept))
    }
    fn now(&self) -> u64 {
        self.world.now()
    }
    fn wait(&self, seconds: u64) -> Result<()> {
        self.world.clock.fetch_add(seconds, SeqCst);
        Ok(())
    }
    fn say(&self, _: &str) {}
}

fn planned<'a>(w: &'a World, path: &str) -> &'a DriveEntry {
    w.drive_plan
        .entries
        .iter()
        .find(|e| e.path == path)
        .unwrap()
}

fn seen(w: &World, generation: &GenerationRef) -> BTreeMap<String, DriveFile> {
    w.drive
        .view(generation)
        .unwrap()
        .files
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect()
}

#[test]
fn interrupted_run_resumes_and_adoption_reaches_a_pc_without_workspace() {
    let w = world();
    // PC A is interrupted after one file.
    w.builds_left.store(1, SeqCst);
    let first = w.run("pc-a", "cache-a");
    assert!(first.stopped);
    assert_eq!(first.switched_now, 1);
    assert_eq!(first.lost, 1, "the plan-time loss is recorded");
    let journal = w.journal("cache-a");
    let status = summarize(&w.drive_plan, &journal.records().unwrap(), None, None);
    assert_eq!((status.to_move, status.switched), (2, 1));
    assert!(!status.ready);
    assert_eq!(status.lost.len(), 1);
    assert_eq!(status.lost[0].original_name, "gone.bin");
    // The old generation is frozen for everyone.
    let known = known_in("p", w.stores(), Some(w.cache("cache-z"))).unwrap();
    assert!(matches!(
        fence(&w.drive_plan.source, &known),
        Fence::Frozen(_)
    ));

    // PC B resumes: only the remaining file is built.
    w.builds_left.store(usize::MAX, SeqCst);
    let second = w.run("pc-b", "cache-b");
    assert_eq!((second.switched_now, second.already_switched), (1, 1));
    let status = summarize(
        &w.drive_plan,
        &w.journal("cache-b").records().unwrap(),
        None,
        None,
    );
    assert!(status.ready);

    // Lost files block adoption unless accepted; nothing is published then.
    let error = w.adopt("cache-b", false).unwrap_err().to_string();
    assert!(
        error.contains("gone.bin") && error.contains("--accept-lost"),
        "{error}"
    );
    assert!(w.drive.published.lock().unwrap().is_empty());
    let adoption = w.adopt("cache-b", true).unwrap();
    assert!(
        w.now() >= 1_000 + SETTLE_SECONDS,
        "waited for the freeze to settle"
    );
    assert_eq!(adoption.files, 3);
    assert_eq!(adoption.dropped, vec!["gone.bin".to_string()]);
    assert_eq!(adoption.epoch, w.drive_plan.epoch);

    // PC C never had a workspace: it finds the adoption in the cloud, a new
    // workspace opens the new epoch, and that generation shows the files.
    let known = known_in("p", w.stores(), Some(w.cache("cache-c"))).unwrap();
    assert_eq!(fresh_workspace(&known).unwrap().epoch, w.drive_plan.epoch);
    let generations = vec![
        Generation {
            epoch: None,
            newest: 99,
            records: 4,
        },
        Generation {
            epoch: Some(w.drive_plan.epoch.clone()),
            newest: 50,
            records: 3,
        },
    ];
    let current = effective(generations, &known);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].epoch, Some(w.drive_plan.epoch.clone()));
    let files = seen(&w, &w.new_generation());
    assert_eq!(files.len(), 3);
    assert!(!files.contains_key("gone.bin"));
    // Unaffected file keeps its archive; moved ones use new drive archives.
    assert_eq!(
        files["docs/kept.txt"].manifest.archive_id,
        planned(&w, "docs/kept.txt").source_archive_id
    );
    for path in ["moved1.bin", "moved2.bin"] {
        let id = &files[path].manifest.archive_id;
        assert!(
            id.starts_with("virtual-") && *id != planned(&w, path).source_archive_id,
            "{path}"
        );
        assert_eq!(files[path].hash, planned(&w, path).hash);
    }
    // A PC still on the old generation is refused with the switch command.
    let message = fence(&w.drive_plan.source, &known).message("p").unwrap();
    assert!(message.contains("--workspace"), "{message}");

    // Adopting again changes nothing.
    let records = w.drive.published.lock().unwrap().clone();
    assert_eq!(w.adopt("cache-c", true).unwrap().epoch, adoption.epoch);
    assert_eq!(*w.drive.published.lock().unwrap(), records);
    let status = summarize(
        &w.drive_plan,
        &w.journal("cache-c").records().unwrap(),
        None,
        Some(adoption),
    );
    assert!(status.adopted.is_some());
}

#[test]
fn files_changed_after_planning_are_caught_up_on_adoption() {
    let mut w = world();
    w.run("pc-a", "cache-a");
    let fresh = payload('5', &["a:x", "b:x", "c:x"]);
    w.cloud.store(&fresh, &[]);
    {
        let mut files = w.drive.files.lock().unwrap();
        // moved1 got a new revision on another PC before the freeze took hold.
        let moved = files.iter_mut().find(|f| f.path == "moved1.bin").unwrap();
        moved.revision = "rev2-moved1.bin".into();
        moved.hash = blake3::hash(b"edited").to_hex().to_string();
        // A new file appeared (unaffected), and gone.bin was deleted.
        files.retain(|f| f.path != "gone.bin");
        files.push(drive_file("new.txt", "rev1-new.txt", fresh));
    }
    let adoption = w.adopt("cache-a", false).unwrap();
    assert_eq!(adoption.files, 4);
    assert!(adoption.dropped.is_empty());
    let files = seen(&w, &w.new_generation());
    assert_eq!(
        files["moved1.bin"].hash,
        blake3::hash(b"edited").to_hex().to_string()
    );
    // moved1 was relocated into a new drive archive; new.txt kept its own.
    let moved = &files["moved1.bin"].manifest.archive_id;
    assert!(moved.starts_with("virtual-") && *moved != payload('2', &TARGET).archive_id);
    assert_eq!(
        files["new.txt"].manifest.archive_id,
        payload('5', &TARGET).archive_id
    );
    // The caught-up file has its own record under the new revision's key.
    let state = progress(&w.journal("cache-a").records().unwrap());
    assert!(matches!(
        state.get(&entry_key("moved1.bin", "rev2-moved1.bin")),
        Some(Progress::Switched(_))
    ));
}

#[test]
fn abandoned_migration_lifts_the_freeze_and_cannot_be_adopted() {
    let w = world();
    w.builds_left.store(0, SeqCst);
    w.run("pc-a", "cache-a");
    let known = known_in("p", w.stores(), Some(w.cache("cache-z"))).unwrap();
    assert_eq!(known.freezes.len(), 1);
    w.journal("cache-a")
        .append(&Record {
            entry: String::new(),
            state: RecordState::Abandoned,
            attempt_id: "x".into(),
            pc_id: "pc-a".into(),
            ts_unix: w.now(),
            new_archive_id: None,
            new_manifest: None,
            losses: vec![],
            detail: None,
        })
        .unwrap();
    let known = known_in("p", w.stores(), Some(w.cache("cache-y"))).unwrap();
    assert!(known.freezes.is_empty());
    assert_eq!(fence(&w.drive_plan.source, &known), Fence::Proceed);
    let error = w.adopt("cache-a", true).unwrap_err().to_string();
    assert!(error.contains("abandoned"), "{error}");
}

#[test]
fn files_claimed_by_a_running_pc_block_adoption() {
    let w = world();
    let journal = w.journal("cache-a");
    let key = planned(&w, "moved1.bin").key.clone();
    journal
        .append(&Record {
            entry: key,
            state: RecordState::Claimed,
            attempt_id: "busy".into(),
            pc_id: "pc-x".into(),
            ts_unix: w.now() + SETTLE_SECONDS,
            new_archive_id: Some("migrate-busy".into()),
            new_manifest: None,
            losses: vec![],
            detail: None,
        })
        .unwrap();
    let error = w.adopt("cache-a", true).unwrap_err().to_string();
    assert!(
        error.contains("moved1.bin") && error.contains("claimed by pc-x"),
        "{error}"
    );
    assert!(w.drive.published.lock().unwrap().is_empty());
    // The adoption froze the source while it waited.
    let known = known_in("p", w.stores(), Some(w.cache("cache-z"))).unwrap();
    assert_eq!(known.freezes.len(), 1);
}

/// Small-file packs migrate as one unit: the pack archive is built once and
/// every member keeps its offset in the new pack.
#[test]
fn a_small_file_pack_moves_once_and_members_keep_their_offsets() {
    let mut cloud = FakeCloud::default();
    let kept = payload('1', &["a:x", "b:x", "c:x"]);
    let moved = payload('2', &["b:x", "c:x", "d:x"]);
    let pack = payload('6', &["b:x", "c:x", "d:x"]);
    for m in [&kept, &moved, &pack] {
        cloud.store(m, &[]);
    }
    cloud.wipe("d:x");
    let member = |path: &str, offset: u64, size: u64| {
        let mut file = drive_file(path, &format!("rev1-{path}"), pack.clone());
        file.size = size;
        file.pack = Some(crate::mount::PackSlice { offset });
        file
    };
    let files = vec![
        drive_file("docs/kept.txt", "rev1-kept", kept.clone()),
        member("small/a.txt", 0, 100),
        drive_file("moved.bin", "rev1-moved", moved.clone()),
        member("small/b.txt", 100, 2_000),
        member("small/c.txt", 2_100, 50_000),
    ];
    let w = world_with(cloud, files);
    // Three units: the kept file, the moved file and ONE pack.
    let paths: Vec<&str> = w
        .drive_plan
        .entries
        .iter()
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    let pack_entry = planned(&w, &format!("(pack {})", pack.archive_id));
    assert_eq!(pack_entry.size, pack.original_size);
    assert!(matches!(
        pack_entry.action,
        Action::Relocate | Action::Reencode
    ));
    assert!(w
        .drive_plan
        .notes
        .iter()
        .any(|n| n.contains("1 small-file pack(s) move as one unit")));

    let run = w.run("pc-a", "cache-a");
    assert_eq!(run.switched_now, 2, "the moved file and the pack, once");
    let adoption = w.adopt("cache-a", false).unwrap();
    assert_eq!(adoption.files, 5);
    assert!(adoption.dropped.is_empty());

    let files = seen(&w, &w.new_generation());
    assert_eq!(files["docs/kept.txt"].manifest.archive_id, kept.archive_id);
    let new_pack = &files["small/a.txt"].manifest.archive_id;
    assert_ne!(new_pack, &pack.archive_id);
    for (path, offset, size) in [
        ("small/a.txt", 0, 100),
        ("small/b.txt", 100, 2_000),
        ("small/c.txt", 2_100, 50_000),
    ] {
        let file = &files[path];
        assert_eq!(
            &file.manifest.archive_id, new_pack,
            "{path} shares the new pack"
        );
        assert_eq!(file.pack.map(|p| p.offset), Some(offset), "{path}");
        assert_eq!(file.size, size, "{path}");
    }
    assert_ne!(files["moved.bin"].manifest.archive_id, *new_pack);
}

/// A lost pack drops every member (only with `--accept-lost`).
#[test]
fn a_lost_pack_drops_all_its_members() {
    let mut cloud = FakeCloud::default();
    let kept = payload('1', &["a:x", "b:x", "c:x"]);
    let pack = payload('7', &["d:x", "d:x", "c:x"]);
    for m in [&kept, &pack] {
        cloud.store(m, &[]);
    }
    cloud.wipe("d:x");
    let member = |path: &str, offset: u64| {
        let mut file = drive_file(path, &format!("rev1-{path}"), pack.clone());
        file.size = 10;
        file.pack = Some(crate::mount::PackSlice { offset });
        file
    };
    let w = world_with(
        cloud,
        vec![
            drive_file("docs/kept.txt", "rev1-kept", kept),
            member("small/x.txt", 0),
            member("small/y.txt", 10),
        ],
    );
    w.run("pc-a", "cache-a");
    let error = w.adopt("cache-a", false).unwrap_err().to_string();
    assert!(
        error.contains("small/x.txt") && error.contains("small/y.txt"),
        "{error}"
    );
    let adoption = w.adopt("cache-a", true).unwrap();
    assert_eq!(adoption.files, 1);
    assert_eq!(
        adoption.dropped,
        vec!["small/x.txt".to_string(), "small/y.txt".to_string()]
    );
}
