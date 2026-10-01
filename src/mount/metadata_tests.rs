//! Checkpoint / compaction tests over in-memory replicas and explicit clocks.
use super::metadata_cache::Cache;
use super::metadata_checkpoint::{pull, survey, Replica};
use super::metadata_checkpoint_model::{Chunk, Family, Head, Packer, FORMAT};
use super::metadata_compaction::{compact, enable_gate, Config, Options, Report};
use super::metadata_dir::{fake::Dir, object_id, ObjectDir};
use super::pool_sync::{list_unseen, read_unseen, EventStore};
use super::shared_model::Event;
use crate::prelude::*;

const DAY: u64 = 86_400;
const T0: u64 = 1_000 * DAY;

impl EventStore for Dir {
    fn missing(&self, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();
        for (id, size) in self.list()? {
            if !known.contains(&id) {
                result.insert(id.clone(), ObjectDir::read(self, &id, size)?);
            }
        }
        Ok(result)
    }
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
        ObjectDir::publish(self, id, bytes)
    }
    fn unseen(&self, known: &BTreeSet<String>) -> Result<Vec<(String, u64)>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|(id, _)| !known.contains(id))
            .collect())
    }
    fn read(&self, id: &str, size: u64) -> Result<Vec<u8>> {
        ObjectDir::read(self, id, size)
    }
}

#[derive(Default)]
struct Fake {
    events: Dir,
    heads: Dir,
    chunks: Dir,
    marks: Dir,
}
impl Fake {
    fn replica(&self) -> Replica<'_> {
        Replica {
            records: vec![&self.events],
            heads: &self.heads,
            chunks: &self.chunks,
            marks: &self.marks,
        }
    }
    fn reads(&self) -> usize {
        self.events.reads.get()
    }
}
fn replicas(fakes: &[Fake]) -> Vec<Replica<'_>> {
    fakes.iter().map(Fake::replica).collect()
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
fn add(fakes: &[Fake], range: std::ops::Range<usize>) {
    for i in range {
        let (id, bytes) = event(i);
        for fake in fakes {
            ObjectDir::publish(&fake.events, &id, &bytes).unwrap();
        }
    }
}
fn config() -> Config {
    Config {
        checkpoint_after_records: 10,
        ..Config::default()
    }
}
fn run(fakes: &[Fake], cache: &mut Cache, now: u64, dry_run: bool, force: bool) -> Report {
    let options = Options {
        now,
        dry_run,
        force,
        config: config(),
    };
    compact(&Family::v6(), &replicas(fakes), cache, &options).unwrap()
}
/// A fresh PC: checkpoints (when needed) plus the tail, like `pull_pool`.
fn bootstrap(fakes: &[Fake]) -> Result<(BTreeMap<String, Event>, usize)> {
    let family = Family::v6();
    let stores: Vec<&dyn EventStore> = fakes.iter().map(|f| &f.events as _).collect();
    let unseen = list_unseen(&stores, &BTreeSet::new())?;
    let mut events = BTreeMap::new();
    let covered = pull(
        &family,
        &replicas(fakes),
        &mut Cache::default(),
        unseen.gate,
        true,
        &mut |_, id, text| {
            let event: Event = serde_json::from_str(text)?;
            event.validate()?;
            events.insert(id.to_owned(), event);
            Ok(())
        },
    )?;
    let before: usize = fakes.iter().map(Fake::reads).sum();
    let covered = covered.get("events").cloned().unwrap_or_default();
    let tail = read_unseen(&stores, &unseen, |id| {
        events.contains_key(id) || covered.contains(id)
    })?;
    let tail_reads = fakes.iter().map(Fake::reads).sum::<usize>() - before;
    events.extend(tail);
    Ok((events, tail_reads))
}

#[test]
fn checkpoint_is_created_verified_replicated_and_incremental() {
    let fakes = [Fake::default(), Fake::default()];
    add(&fakes, 0..30);
    let mut cache = Cache::default();
    let report = run(&fakes, &mut cache, T0, false, false);
    assert_eq!((report.records, report.checkpointed_records), (30, 30));
    let head = report.checkpoint.clone().unwrap();
    for fake in &fakes {
        assert_eq!(
            *fake.heads.objects.borrow(),
            *fakes[0].heads.objects.borrow()
        );
        assert_eq!(
            *fake.chunks.objects.borrow(),
            *fakes[0].chunks.objects.borrow()
        );
        let bytes = fake.heads.objects.borrow()[&head].clone();
        let parsed = Head::parse(&head, &bytes, &Family::v6()).unwrap();
        for chunk in &parsed.chunks {
            let bytes = fake.chunks.objects.borrow()[chunk].clone();
            Chunk::parse(chunk, &bytes, &Family::v6()).unwrap();
        }
    }
    // Nothing new: no further checkpoint, everything covered.
    let again = run(&fakes, &mut cache, T0 + 1, false, false);
    assert_eq!((again.uncovered, again.checkpoint), (0, None));
    // Below the threshold only a manual (forced) run checkpoints.
    add(&fakes, 30..35);
    assert_eq!(run(&fakes, &mut cache, T0 + 2, false, false).uncovered, 5);
    let forced = run(&fakes, &mut cache, T0 + 3, false, true);
    assert_eq!(forced.checkpointed_records, 5);
    let survey = survey(
        &Family::v6(),
        &replicas(&fakes),
        &mut cache,
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    let (newest, newest_head) = survey.newest().unwrap();
    assert_eq!(Some(newest.clone()), forced.checkpoint);
    // The new head keeps every chunk of the old one.
    assert!(survey.heads[&head]
        .chunks
        .iter()
        .all(|c| newest_head.chunks.contains(c)));
    // A dry run writes nothing.
    add(&fakes, 35..50);
    let heads = fakes[0].heads.objects.borrow().len();
    let dry = run(&fakes, &mut cache, T0 + 4, true, false);
    assert_eq!((dry.checkpointed_records, dry.checkpoint), (15, None));
    assert_eq!(fakes[0].heads.objects.borrow().len(), heads);
}

#[test]
fn bootstrap_reads_checkpoint_plus_only_the_tail() {
    let fakes = [Fake::default(), Fake::default()];
    add(&fakes, 0..30);
    run(&fakes, &mut Cache::default(), T0, false, false);
    add(&fakes, 30..35);
    let (events, tail_reads) = bootstrap(&fakes).unwrap();
    assert_eq!(events.len(), 35);
    assert_eq!(tail_reads, 5);
    super::shared_model::reduce(&events).unwrap();
}

#[test]
fn bootstrap_without_checkpoint_streams_beyond_the_old_limit() {
    let fake = Fake::default();
    add(std::slice::from_ref(&fake), 0..10_001);
    let (events, reads) = bootstrap(std::slice::from_ref(&fake)).unwrap();
    assert_eq!((events.len(), reads), (10_001, 10_001));
}

#[test]
fn corrupt_or_partial_checkpoints_are_ignored_unless_deletion_is_on() {
    let fakes = [Fake::default()];
    add(&fakes, 0..12);
    let report = run(&fakes, &mut Cache::default(), T0, false, false);
    let head = report.checkpoint.unwrap();
    // Corrupt the only chunk: its bytes no longer match its id.
    let chunk = fakes[0]
        .chunks
        .objects
        .borrow()
        .keys()
        .next()
        .cloned()
        .unwrap();
    fakes[0]
        .chunks
        .objects
        .borrow_mut()
        .insert(chunk.clone(), b"{}".to_vec());
    // A head whose chunk does not exist, and a head with garbage bytes.
    let orphan = Head {
        format: FORMAT,
        family: "v6".into(),
        created_unix: T0,
        chunks: vec!["e".repeat(64)],
        records: 1,
        bytes: 1,
    };
    let (orphan_id, orphan_bytes) = super::metadata_checkpoint_model::head_bytes(&orphan).unwrap();
    ObjectDir::publish(&fakes[0].heads, &orphan_id, &orphan_bytes).unwrap();
    fakes[0]
        .heads
        .objects
        .borrow_mut()
        .insert("f".repeat(64), b"garbage".to_vec());
    let survey = survey(
        &Family::v6(),
        &replicas(&fakes),
        &mut Cache::default(),
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    assert!(survey.heads.is_empty());
    assert_eq!(survey.broken, 3);
    assert!(!survey.heads.contains_key(&head));
    // Gate absent: nothing was deleted, the tail still has every record.
    let (events, reads) = bootstrap(&fakes).unwrap();
    assert_eq!((events.len(), reads), (12, 12));
    // Gate present: fail closed instead of showing a possibly partial drive.
    enable_gate(&Family::v6(), &replicas(&fakes)).unwrap();
    assert!(bootstrap(&fakes)
        .unwrap_err()
        .to_string()
        .contains("retain this workspace"));
}

#[test]
fn deletion_needs_gate_grace_and_checkpoint_on_every_replica() {
    let fakes = [Fake::default(), Fake::default()];
    add(&fakes, 0..20);
    let mut cache = Cache::default();
    let first = run(&fakes, &mut cache, T0, false, false);
    assert!(!first.deletion_enabled);
    assert_eq!((first.marked, first.marks), (0, 0));
    // Without the gate nothing is ever marked or deleted.
    assert_eq!(
        run(&fakes, &mut cache, T0 + 100 * DAY, false, false).deleted,
        0
    );
    assert_eq!(fakes[1].events.objects.borrow().len(), 20);

    enable_gate(&Family::v6(), &replicas(&fakes)).unwrap();
    let marked = run(&fakes, &mut cache, T0, false, false);
    assert!(marked.deletion_enabled);
    assert_eq!(marked.marked, 20);
    let grace = Config::default().grace_seconds();
    assert_eq!(marked.next_deletion_unix, Some(T0 + grace));
    // Records written after the mark are not in it.
    add(&fakes, 20..22);
    // Before the grace period: nothing deleted.
    let early = run(&fakes, &mut cache, T0 + grace - 1, false, false);
    assert_eq!((early.deletable, early.deleted), (0, 0));
    // The checkpoint missing on one replica blocks deletion.
    let chunk = fakes[1]
        .chunks
        .objects
        .borrow()
        .keys()
        .next()
        .cloned()
        .unwrap();
    let saved = fakes[1].chunks.objects.borrow_mut().remove(&chunk).unwrap();
    let partial = run(&fakes, &mut cache, T0 + grace, false, false);
    assert_eq!(partial.deleted, 0);
    assert!(partial.notes.iter().any(|n| n.contains("every replica")));
    fakes[1].chunks.objects.borrow_mut().insert(chunk, saved);
    // A dry run only reports.
    let dry = run(&fakes, &mut cache, T0 + grace, true, false);
    assert_eq!((dry.deletable, dry.deleted), (20, 0));
    assert_eq!(fakes[0].events.objects.borrow().len(), 23);
    // Due: the marked records go from every replica; the gate, the two newer
    // records and the checkpoint stay; the mark is consumed.
    let done = run(&fakes, &mut cache, T0 + grace, false, false);
    assert_eq!(done.deleted, 20);
    for fake in &fakes {
        assert_eq!(fake.events.objects.borrow().len(), 3);
        assert!(fake.events.objects.borrow().contains_key(&event(20).0));
        assert!(fake.marks.objects.borrow().is_empty());
        assert!(!fake.heads.objects.borrow().is_empty());
    }
    // A fresh PC still sees every file: 20 from the checkpoint, 2 in the tail.
    let (events, tail_reads) = bootstrap(&fakes).unwrap();
    assert_eq!((events.len(), tail_reads), (22, 2));
}

#[test]
fn concurrent_heads_merge_and_superseded_heads_are_removed_after_deletion() {
    let fakes = [Fake::default()];
    add(&fakes, 0..10);
    let mut cache = Cache::default();
    let a = run(&fakes, &mut cache, T0, false, false)
        .checkpoint
        .unwrap();
    // A second PC checkpointed other records without seeing head `a`.
    let family = Family::v6();
    let mut packer = Packer::new(&family);
    let (id, bytes) = event(99);
    for fake in &fakes {
        ObjectDir::publish(&fake.events, &id, &bytes).unwrap();
    }
    let mut ready = vec![];
    assert!(packer
        .push("events", &id, String::from_utf8(bytes).unwrap(), &mut ready)
        .unwrap());
    let (chunk_id, chunk) = packer.flush().unwrap().unwrap();
    ObjectDir::publish(&fakes[0].chunks, &chunk_id, &chunk).unwrap();
    let fork = Head {
        format: FORMAT,
        family: "v6".into(),
        created_unix: T0 + 1,
        chunks: vec![chunk_id],
        records: 1,
        bytes: 1,
    };
    let (b, b_bytes) = super::metadata_checkpoint_model::head_bytes(&fork).unwrap();
    ObjectDir::publish(&fakes[0].heads, &b, &b_bytes).unwrap();
    let forked = survey(
        &Family::v6(),
        &replicas(&fakes),
        &mut cache,
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    assert!(forked.newest().is_none(), "no head includes the other");
    // The next checkpoint merges both forks.
    add(&fakes, 10..12);
    let merged = run(&fakes, &mut cache, T0 + 2, false, true);
    let c = merged.checkpoint.unwrap();
    assert_eq!(merged.uncovered, 0);
    enable_gate(&Family::v6(), &replicas(&fakes)).unwrap();
    assert_eq!(run(&fakes, &mut cache, T0 + 3, false, false).marked, 13);
    let done = run(
        &fakes,
        &mut cache,
        T0 + 3 + Config::default().grace_seconds(),
        false,
        false,
    );
    assert_eq!((done.deleted, done.checkpoints_removed), (13, 2));
    let heads = fakes[0].heads.objects.borrow();
    assert!(heads.contains_key(&c) && !heads.contains_key(&a) && !heads.contains_key(&b));
}

#[test]
fn gate_breaks_old_clients_loudly_and_new_clients_skip_it() {
    let v6 = Family::v6();
    assert_eq!(
        v6.gate_id(),
        Family::v6().gate_id(),
        "fixed bytes, fixed id"
    );
    // An older RPool parses the gate as an event and refuses it.
    let gate: Event = serde_json::from_slice(&v6.gate).unwrap();
    assert!(gate
        .validate()
        .unwrap_err()
        .to_string()
        .contains("unsupported namespace event version"));
    // v7: a snapshot of another version stops the old analysis.
    let v7 = Family::v7();
    let snapshot: super::peer_snapshot_model::Snapshot = serde_json::from_slice(&v7.gate).unwrap();
    assert_eq!(snapshot.id().unwrap(), v7.gate_id());
    let all = [(v7.gate_id(), snapshot)].into_iter().collect();
    let policy = super::peer_snapshot_model::Policy {
        genesis_id: "1".repeat(64),
        history_limit: 0,
    };
    assert!(super::peer_snapshot_model::analyze(&all, &policy).is_err());
    // New clients skip it and report it.
    let fakes = [Fake::default()];
    add(&fakes, 0..2);
    enable_gate(&v6, &replicas(&fakes)).unwrap();
    let stores: Vec<&dyn EventStore> = vec![&fakes[0].events];
    let unseen = list_unseen(&stores, &BTreeSet::new()).unwrap();
    assert!(unseen.gate);
    assert_eq!(unseen.entries.len(), 2);
    // A chunk must never carry the gate.
    let chunk = Chunk {
        format: FORMAT,
        family: "v6".into(),
        records: [(
            "events".to_string(),
            [(v6.gate_id(), String::from_utf8(v6.gate.clone()).unwrap())].into(),
        )]
        .into(),
    };
    let bytes = serde_json::to_vec(&chunk).unwrap();
    assert!(Chunk::parse(&object_id(&bytes), &bytes, &v6).is_err());
    // Other format/family numbers are not interpreted.
    let mut future = chunk;
    future.format = 2;
    let bytes = serde_json::to_vec(&future).unwrap();
    assert!(Chunk::parse(&object_id(&bytes), &bytes, &v6).is_err());
}

#[test]
fn oversize_records_stay_outside_chunks_and_unreachable_replicas_fail() {
    let family = Family::v6();
    let mut packer = Packer::new(&family);
    let mut ready = vec![];
    let big = "x".repeat(super::metadata_checkpoint_model::CHUNK_TARGET);
    assert!(!packer
        .push("events", &object_id(big.as_bytes()), big, &mut ready)
        .unwrap());
    assert!(packer.flush().unwrap().is_none());
    // Records of ~1 MiB split across several chunks below the object limit.
    for i in 0..20 {
        let text = format!("{i}{}", "y".repeat(1 << 20));
        packer
            .push("events", &object_id(text.as_bytes()), text, &mut ready)
            .unwrap();
    }
    ready.extend(packer.flush().unwrap());
    assert!(ready.len() >= 3);
    assert!(ready
        .iter()
        .all(|(_, b)| b.len() <= super::metadata_limits::RECORD_BYTES_MAX));
    // An unreachable replica is an error, never an empty namespace.
    let fakes = [Fake::default(), Fake::default()];
    add(&fakes, 0..3);
    fakes[1].events.fail.set(true);
    let options = Options {
        now: T0,
        dry_run: false,
        force: true,
        config: config(),
    };
    assert!(compact(
        &Family::v6(),
        &replicas(&fakes),
        &mut Cache::default(),
        &options
    )
    .is_err());
    assert!(bootstrap(&fakes).is_err());
}

#[test]
fn growth_alert_thresholds() {
    use super::metadata_limits::WARN_RECORDS;
    let report = |uncovered| {
        Ok(Some(Report {
            uncovered,
            ..Default::default()
        }))
    };
    let alert = |r| super::metadata_pool::growth_alert("p", &r, 5);
    assert!(alert(report(WARN_RECORDS - 1)).is_none());
    let growing = alert(report(WARN_RECORDS)).unwrap();
    assert_eq!(
        growing.kind,
        crate::monitor::model::AlertKind::MetadataGrowing
    );
    assert!(growing.message.contains("rpool pool compact p"));
    assert!(alert(Err(anyhow!("offline")))
        .unwrap()
        .message
        .contains("failed"));
    assert!(alert(Ok(None)).is_none());
}

#[test]
fn config_defaults_and_validation() {
    let config = Config::default();
    assert!(config.auto);
    assert_eq!(config.grace_seconds(), 14 * DAY);
    config.validate().unwrap();
    let bad = Config {
        grace_days: 0,
        ..Config::default()
    };
    assert!(bad.validate().is_err());
    let parsed: Config = serde_json::from_str(r#"{"grace_days": 30}"#).unwrap();
    assert_eq!(
        (parsed.grace_days, parsed.checkpoint_after_records),
        (30, 2_000)
    );
    assert!(serde_json::from_str::<Config>(r#"{"unknown": 1}"#).is_err());
}
