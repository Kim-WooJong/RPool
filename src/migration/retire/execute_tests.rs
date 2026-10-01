use super::*;
use crate::migration::model::RecordState;
use crate::migration::retire::model::{RetireOptions, RetireStep};
use crate::migration::retire::restore::restore;
use crate::migration::retire::test_fixture::{fixture, FakeIo, O1};
use std::sync::atomic::Ordering::SeqCst;

const DAY: u64 = 86_400;

fn confirm() -> RetireOptions {
    RetireOptions {
        confirm: true,
        ..RetireOptions::default()
    }
}

fn count(io: &FakeIo, kind: RetireKind) -> usize {
    io.kinds().iter().filter(|(k, _)| *k == kind).count()
}

#[test]
fn dry_run_changes_nothing() {
    let io = FakeIo::new(fixture());
    let report = retire(&io, &RetireOptions::default()).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.candidates.len(), 3);
    assert!(report.quarantine.is_empty());
    assert!(io.journal.lock().unwrap().is_empty());
    assert!(io.deleted.lock().unwrap().is_empty());
    assert!(io.forgotten.lock().unwrap().is_empty());
    // Per account: every root holds a manifest replica of each original.
    assert_eq!(report.candidate_accounts.len(), 3);
    let total: u64 = report.candidate_accounts.iter().map(|a| a.objects).sum();
    assert_eq!(total, 8 + 6 + 1);
    assert!(report.guard.quarantine_refusal.is_none());
    assert!(report.guard.pool_objects > total);
}

#[test]
fn incomplete_or_abandoned_migrations_are_refused() {
    let mut f = fixture();
    f.records
        .retain(|r| !(r.entry == "old2" && r.state == RecordState::Switched));
    let io = FakeIo::new(f);
    let error = retire(&io, &confirm()).unwrap_err().to_string();
    assert!(error.contains("not complete"), "{error}");
    let mut f = fixture();
    f.records
        .push(crate::migration::execute::tests_support::rec(
            "",
            RecordState::Abandoned,
            50,
            "pc1",
        ));
    let io = FakeIo::new(f);
    assert!(retire(&io, &confirm())
        .unwrap_err()
        .to_string()
        .contains("abandoned"));
    assert_eq!(io.observed.load(SeqCst), 0);
}

#[test]
fn fossil_grace_then_delete() {
    let io = FakeIo::new(fixture());
    let report = retire(&io, &confirm()).unwrap();
    assert_eq!(count(&io, RetireKind::Fossil), 3);
    assert!(
        io.deleted.lock().unwrap().is_empty(),
        "quarantine deletes nothing"
    );
    assert_eq!(report.quarantine.len(), 3);
    assert!(report.candidates.is_empty());
    assert!(report
        .quarantine
        .iter()
        .all(|v| v.state == FossilState::Waiting));
    // Before the grace: nothing happens, the second run adds no records.
    io.clock.fetch_add(7 * DAY - 1, SeqCst);
    let before = io.journal.lock().unwrap().len();
    retire(&io, &confirm()).unwrap();
    assert_eq!(io.journal.lock().unwrap().len(), before);
    assert!(io.deleted.lock().unwrap().is_empty());
    // After the grace: deleted, manifests first, inventory entries dropped.
    io.clock.fetch_add(1, SeqCst);
    let report = retire(&io, &confirm()).unwrap();
    let deleted = io.deleted.lock().unwrap().clone();
    assert_eq!(deleted.len(), 15);
    let old1: Vec<&String> = deleted.iter().filter(|d| d.contains(":old1/")).collect();
    assert_eq!(old1.len(), 8);
    assert!(
        old1[..3].iter().all(|d| d.ends_with("/manifest.json")),
        "manifests first"
    );
    assert_eq!(count(&io, RetireKind::Purged), 3);
    assert_eq!(count(&io, RetireKind::Deleting), 3);
    assert_eq!(report.purged, 3);
    assert!(report.quarantine.is_empty() && report.candidates.is_empty());
    let mut forgotten = io.forgotten.lock().unwrap().clone();
    forgotten.sort();
    assert_eq!(forgotten, vec![O1, "old1", "old2"]);
    // Every record says who and when; Deleted batches list the objects.
    let journal = io.journal.lock().unwrap().clone();
    assert!(journal
        .iter()
        .all(|r| r.pc_id == "pc-test" && r.ts_unix >= 1_000));
    let listed: usize = journal
        .iter()
        .filter(|r| r.kind == RetireKind::Deleted)
        .map(|r| r.objects.len())
        .sum();
    assert_eq!(listed, 15);
    // Idempotent: another run does nothing more.
    let before = journal.len();
    retire(&io, &confirm()).unwrap();
    assert_eq!(io.journal.lock().unwrap().len(), before);
}

#[test]
fn grace_is_configurable_and_steps_are_separate() {
    let io = FakeIo::new(fixture());
    let options = RetireOptions {
        grace_seconds: 60,
        step: RetireStep::Quarantine,
        ..confirm()
    };
    retire(&io, &options).unwrap();
    io.clock.fetch_add(60, SeqCst);
    // Quarantine-only runs never delete.
    retire(&io, &options).unwrap();
    assert!(io.deleted.lock().unwrap().is_empty());
    let report = retire(&io, &RetireOptions::default()).unwrap();
    assert!(report
        .quarantine
        .iter()
        .all(|v| v.state == FossilState::Due));
    let delete = RetireOptions {
        step: RetireStep::Delete,
        ..confirm()
    };
    retire(&io, &delete).unwrap();
    assert_eq!(io.deleted.lock().unwrap().len(), 15);
}

#[test]
fn restore_during_grace() {
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    io.clock.fetch_add(DAY, SeqCst);
    let restored = restore(&io, &["old1".to_string()], false).unwrap();
    assert_eq!(restored, vec!["old1"]);
    let report = retire(&io, &RetireOptions::default()).unwrap();
    assert_eq!(report.quarantine.len(), 2);
    assert_eq!(
        report.candidates.len(),
        1,
        "a restored item is a candidate again"
    );
    // After the grace only the two still quarantined items are deleted.
    io.clock.fetch_add(7 * DAY, SeqCst);
    let delete = RetireOptions {
        step: RetireStep::Delete,
        ..confirm()
    };
    retire(&io, &delete).unwrap();
    assert!(!io
        .deleted
        .lock()
        .unwrap()
        .iter()
        .any(|d| d.contains(":old1/")));
    // Unknown or finished items cannot be restored.
    assert!(restore(&io, &["old2".to_string()], false).is_err());
    assert!(restore(&io, &["nope".to_string()], false).is_err());
    // Quarantined again: a new fossil generation with a fresh grace.
    retire(&io, &confirm()).unwrap();
    let report = retire(&io, &RetireOptions::default()).unwrap();
    assert_eq!(report.quarantine.len(), 1);
    assert_eq!(report.quarantine[0].state, FossilState::Waiting);
    assert_eq!(restore(&io, &[], true).unwrap(), vec!["old1"]);
}

#[test]
fn re_reference_during_grace_cancels_deletion() {
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    // Someone imported old1 into the drive during the grace.
    io.world.lock().unwrap().refs.add_json(
        "drive v6 metadata",
        &serde_json::json!({"archive_id": "old1"}),
    );
    io.clock.fetch_add(8 * DAY, SeqCst);
    let dry = retire(&io, &RetireOptions::default()).unwrap();
    let old1 = dry
        .quarantine
        .iter()
        .find(|v| v.item.archive_id == "old1")
        .unwrap();
    assert!(old1
        .blocked
        .as_deref()
        .unwrap()
        .contains("still referenced"));
    let report = retire(&io, &confirm()).unwrap();
    assert!(!io
        .deleted
        .lock()
        .unwrap()
        .iter()
        .any(|d| d.contains(":old1/")));
    assert_eq!(count(&io, RetireKind::Cancelled), 1);
    assert_eq!(report.purged, 2);
    assert!(report
        .kept
        .iter()
        .any(|k| k.archive_id == "old1" && k.reason == KeepReason::Referenced));
}

#[test]
fn uncertain_references_postpone_deletion() {
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    io.world
        .lock()
        .unwrap()
        .refs
        .uncertain("drive metadata: offline".into());
    io.clock.fetch_add(8 * DAY, SeqCst);
    let report = retire(&io, &confirm()).unwrap();
    assert!(io.deleted.lock().unwrap().is_empty());
    assert_eq!(
        count(&io, RetireKind::Cancelled),
        0,
        "postponed, still quarantined"
    );
    assert_eq!(report.quarantine.len(), 3);
    assert!(report.actions.iter().all(|a| a.starts_with("postponed")));
}

#[test]
fn changed_objects_cancel_deletion() {
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    if let Some(crate::migration::enumerate::RemoteListing::Listed(files)) =
        io.world.lock().unwrap().listings.get_mut("b:")
    {
        files.insert(format!("{O1}/data/late.bin"), 4);
    }
    io.clock.fetch_add(8 * DAY, SeqCst);
    retire(&io, &confirm()).unwrap();
    assert!(!io.deleted.lock().unwrap().iter().any(|d| d.contains(O1)));
    assert_eq!(count(&io, RetireKind::Cancelled), 1);
}

#[test]
fn mass_delete_guard() {
    let io = FakeIo::new(fixture());
    let strict = RetireOptions {
        max_delete_objects: 5,
        ..confirm()
    };
    let error = retire(&io, &strict).unwrap_err().to_string();
    assert!(
        error.contains("mass-delete guard") && error.contains("limit of 5"),
        "{error}"
    );
    assert!(
        io.journal.lock().unwrap().is_empty(),
        "refused before any change"
    );
    let percent = RetireOptions {
        max_delete_percent: 1,
        ..confirm()
    };
    assert!(retire(&io, &percent).is_err());
    let dry = retire(
        &io,
        &RetireOptions {
            confirm: false,
            ..strict.clone()
        },
    )
    .unwrap();
    assert!(dry.guard.quarantine_refusal.is_some());
    // Forced: quarantined; the delete step is guarded again later.
    retire(
        &io,
        &RetireOptions {
            force: true,
            ..strict.clone()
        },
    )
    .unwrap();
    assert_eq!(count(&io, RetireKind::Fossil), 3);
    io.clock.fetch_add(8 * DAY, SeqCst);
    assert!(retire(&io, &strict).is_err());
    assert!(io.deleted.lock().unwrap().is_empty());
    retire(
        &io,
        &RetireOptions {
            force: true,
            ..strict
        },
    )
    .unwrap();
    assert_eq!(io.deleted.lock().unwrap().len(), 15);
}

#[test]
fn interrupted_deletion_resumes() {
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    io.clock.fetch_add(8 * DAY, SeqCst);
    *io.fail_after.lock().unwrap() = Some(2);
    assert!(retire(&io, &confirm()).is_err());
    // The orphan (one object) was purged, old1 stopped after one object.
    assert_eq!(io.deleted.lock().unwrap().len(), 2);
    assert_eq!(count(&io, RetireKind::Deleting), 2);
    assert_eq!(count(&io, RetireKind::Purged), 1);
    let report = retire(&io, &RetireOptions::default()).unwrap();
    let deleting: Vec<_> = report
        .quarantine
        .iter()
        .filter(|v| v.state == FossilState::Deleting)
        .collect();
    assert_eq!(deleting.len(), 1);
    // Point of no return: a deleting item cannot be restored.
    let id = deleting[0].item.archive_id.clone();
    assert!(restore(&io, std::slice::from_ref(&id), false).is_err());
    // The next run resumes it (already deleted objects are fine) and continues.
    let report = retire(&io, &confirm()).unwrap();
    assert_eq!(report.purged, 3);
    let deleted = io.deleted.lock().unwrap().clone();
    let unique: BTreeSet<_> = deleted.iter().collect();
    assert_eq!(unique.len(), 15);
}

#[test]
fn restore_racing_the_start_of_deletion_wins() {
    struct Racing(FakeIo);
    impl RetireIo for Racing {
        fn plan(&self) -> Result<Plan> {
            self.0.plan()
        }
        fn records(&self) -> Result<Vec<Record>> {
            self.0.records()
        }
        fn retire_records(&self) -> Result<Vec<RetireRecord>> {
            self.0.retire_records()
        }
        fn append(&self, record: &RetireRecord) -> Result<()> {
            self.0.append(record)?;
            // Another PC restores right after this PC wrote Deleting.
            if record.kind == RetireKind::Deleting {
                let mut restore = record.clone();
                restore.kind = RetireKind::Restore;
                restore.pc_id = "other-pc".into();
                self.0.append(&restore)?;
            }
            Ok(())
        }
        fn observe(&self, p: &Plan, r: &[Record], o: &RetireOptions) -> Result<World> {
            self.0.observe(p, r, o)
        }
        fn delete(&self, address: &str) -> Result<()> {
            self.0.delete(address)
        }
        fn forget(&self, id: &str) -> Result<()> {
            self.0.forget(id)
        }
        fn now(&self) -> u64 {
            self.0.now()
        }
        fn new_id(&self) -> Result<String> {
            self.0.new_id()
        }
        fn pc_id(&self) -> String {
            self.0.pc_id()
        }
        fn say(&self, _: &str) {}
    }
    let io = Racing(FakeIo::new(fixture()));
    retire(&io, &confirm()).unwrap();
    io.0.clock.fetch_add(8 * DAY, SeqCst);
    retire(
        &io,
        &RetireOptions {
            step: RetireStep::Delete,
            ..confirm()
        },
    )
    .unwrap();
    assert!(io.0.deleted.lock().unwrap().is_empty());
    assert_eq!(count(&io.0, RetireKind::Cancelled), 3);
    let report = retire(&io.0, &RetireOptions::default()).unwrap();
    assert!(report.quarantine.is_empty());
    assert_eq!(report.candidates.len(), 3);
}

#[test]
fn journal_keeps_retire_records_apart_from_migration_records() {
    use crate::migration::journal::{Journal, JournalStore, LocalStore, RETIRE};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let cloud: Arc<dyn JournalStore> = Arc::new(LocalStore::new(dir.path().join("cloud")));
    let cache: Arc<dyn JournalStore> = Arc::new(LocalStore::new(dir.path().join("cache")));
    let journal = Journal::with_stores("p", "mig", vec![cloud.clone()], Some(cache)).unwrap();
    let f = fixture();
    journal.publish_plan(&f.plan).unwrap();
    journal.append(&f.records[0]).unwrap();
    let io = FakeIo::new(fixture());
    retire(&io, &confirm()).unwrap();
    for record in io.journal.lock().unwrap().iter() {
        journal.append_in(RETIRE, record).unwrap();
        journal.append_in(RETIRE, record).unwrap(); // idempotent
    }
    assert_eq!(journal.records().unwrap().len(), 1);
    let back: Vec<RetireRecord> = journal.records_in(RETIRE).unwrap();
    assert_eq!(back.len(), 3);
    // Another PC (no cache) sees the same records from the cloud.
    let other = Journal::with_stores("p", "mig", vec![cloud], None).unwrap();
    let mut seen: Vec<RetireRecord> = other.records_in(RETIRE).unwrap();
    seen.sort_by(|a, b| a.item.cmp(&b.item));
    assert_eq!(
        seen.iter().map(|r| r.item.as_str()).collect::<Vec<_>>(),
        vec![O1, "old1", "old2"]
    );
    assert!(seen
        .iter()
        .all(|r| r.kind == RetireKind::Fossil && r.pc_id == "pc-test"));
}
