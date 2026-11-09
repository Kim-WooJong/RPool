//! Deterministic off-lock projection, coherent reporting and optimistic commit tests.
use super::*;
use crate::mount::crash::projection_hook;
use crate::mount::virtual_tests::content;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Clear hooks on assertion failure too, before this test thread is reused.
struct Hook;
impl Drop for Hook {
    fn drop(&mut self) {
        projection_hook::clear();
    }
}
fn drive() -> (tempfile::TempDir, Arc<VirtualDrive>) {
    let root = tempfile::tempdir().unwrap();
    let mut d = fixture(root.path());
    d.pool_sync_roots = vec!["synthetic:metadata".into()];
    d.state.lock().unwrap().version = 6;
    (root, Arc::new(d))
}
fn event(path: &str, bytes: &[u8], parents: Vec<String>, worker: &str) -> Event {
    Event {
        version: 1,
        path: path.into(),
        worker: worker.into(),
        device: worker.into(),
        parents,
        content: Some(content(bytes)),
    }
}
fn insert(d: &VirtualDrive, e: Event) -> String {
    let id = e.id().unwrap();
    d.state.lock().unwrap().events.insert(id.clone(), e);
    id
}
fn write(d: &VirtualDrive, path: &str, bytes: &[u8]) -> Intent {
    let i = d.begin(path).unwrap();
    fs::write(d.spool_path(&i), bytes).unwrap();
    d.seal(i).unwrap();
    d.state.lock().unwrap().pending.last().unwrap().clone()
}

#[test]
fn status_capacity_share_full_projection_without_state_lock() {
    let (_root, d) = drive();
    let base = insert(&d, event("file", b"base", vec![], "base"));
    insert(&d, event("file", b"one", vec![base.clone()], "one"));
    insert(&d, event("file", b"two", vec![base], "two"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (count, observer) = (calls.clone(), d.clone());
    projection_hook::set(move || {
        assert!(
            observer.state.try_lock().is_some(),
            "projection holds StateLock"
        );
        count.fetch_add(1, Ordering::SeqCst);
    });
    let _hook = Hook;
    assert_eq!(d.pool_status().unwrap().conflicts.len(), 1);
    let mut capacity = CapacityStatus::default();
    d.fill_namespace_capacity(&mut capacity).unwrap();
    assert_eq!(capacity.committed_logical_used, Some(10));
    assert_eq!(capacity.logical_used, 10);
    assert_eq!(capacity.namespace_event_ids.len(), 3);
    assert_eq!(capacity.conflicts.len(), 1);
    d.pool_status().unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "idle status/capacity must reuse full projection"
    );
}

#[test]
fn capacity_fields_use_one_snapshot_during_concurrent_mutation() {
    let (_root, d) = drive();
    let before = write(&d, "pending", b"12345");
    let old = insert(&d, event("cloud", b"old", vec![], "old"));
    let observer = d.clone();
    projection_hook::set(move || {
        let mut s = observer.state.try_lock().expect("capacity holds StateLock");
        s.pending.clear();
        s.events.clear();
    });
    let _hook = Hook;
    let mut capacity = CapacityStatus::default();
    d.fill_namespace_capacity(&mut capacity).unwrap();
    assert_eq!(capacity.committed_logical_used, Some(3));
    assert_eq!(capacity.logical_used, 8);
    assert_eq!(capacity.pending_writes, 1);
    assert_eq!(capacity.pending_ids, [before.id]);
    assert_eq!(capacity.namespace_event_ids, [old]);
    assert_eq!(
        capacity.additional_estimate, 0,
        "pending has no verified placement budget"
    );
    assert!(d.state.lock().unwrap().pending.is_empty());
}

#[test]
fn commit_retries_unsaved_mutations_and_keeps_warm_visible_cache() {
    let (root, d) = drive();
    let intent = write(&d, "first", b"first");
    let other = write(&d, "second", b"second");
    let peer = event("peer", b"peer", vec![], "peer");
    let peer_id = peer.id().unwrap();
    let observer = d.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let peer_key = peer_id.clone();
    projection_hook::set(move || {
        let mut s = observer
            .state
            .try_lock()
            .expect("commit projects under StateLock");
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            // No save/generation change: only the StateLock mutation token detects this.
            s.events.insert(peer_key.clone(), peer.clone());
            s.published.insert(peer_key.clone());
            s.bases.insert("peer".into(), vec![peer_key.clone()]);
            s.directories.insert("empty".into());
            s.pending.last_mut().unwrap().size = 7;
        }
    });
    let _hook = Hook;
    d.commit_uploaded(&intent, Some(content(b"first"))).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let visible = d.visible().unwrap();
    assert!(visible.file("first").is_some());
    assert_eq!(visible.file("second").unwrap().1, 7);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "first visible call must stay warm"
    );
    projection_hook::clear();
    let loaded = Namespace::load(root.path(), "tester").unwrap();
    assert!(loaded.events.contains_key(&peer_id));
    assert!(loaded.published.contains(&peer_id));
    assert_eq!(loaded.bases["peer"], [peer_id]);
    assert!(loaded.directories.contains("empty"));
    assert_eq!(loaded.pending[0].id, other.id);
    assert_eq!(loaded.pending[0].size, 7);
    assert!(loaded.committed_intents.contains_key(&intent.id));
}

#[test]
fn four_concurrent_commits_project_once_each_and_preserve_receipts() {
    let (root, d) = drive();
    let intents: Vec<_> = (0..4)
        .map(|n| write(&d, &format!("file-{n}"), b"data"))
        .collect();
    let start = Arc::new(std::sync::Barrier::new(5));
    let calls = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        let mut jobs = Vec::new();
        for intent in &intents {
            let (d, start, count) = (d.clone(), start.clone(), calls.clone());
            jobs.push(scope.spawn(move || {
                let observer = d.clone();
                projection_hook::set(move || {
                    assert!(observer.state.try_lock().is_some());
                    count.fetch_add(1, Ordering::SeqCst);
                });
                let _hook = Hook;
                start.wait();
                d.commit_uploaded(intent, Some(content(b"data")))
            }));
        }
        start.wait();
        for job in jobs {
            job.join().unwrap().unwrap();
        }
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "uploader gate prevents wasted attempt projections"
    );
    let loaded = Namespace::load(root.path(), "tester").unwrap();
    assert!(loaded.pending.is_empty());
    assert_eq!(loaded.events.len(), 4);
    assert_eq!(loaded.committed_intents.len(), 4);
    for intent in &intents {
        let event = &loaded.events[&loaded.committed_intents[&intent.id]];
        assert_eq!(event.path, intent.path);
        assert_eq!(event.content.as_ref().unwrap().hash, intent.hash);
    }
    assert_eq!(d.visible_used().unwrap(), 16);
}

#[test]
fn unrelated_changes_on_every_preparation_rebase_without_starvation() {
    let (root, d) = drive();
    let seed = insert(&d, event("seed", b"seed", vec![], "seed"));
    let first = write(&d, "first", b"first");
    let other = write(&d, "other", b"other");
    let observer = d.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let seed_key = seed.clone();
    projection_hook::set(move || {
        let mut s = observer
            .state
            .try_lock()
            .expect("preparation holds StateLock");
        s.pending.last_mut().unwrap().size = 6;
        s.published.insert(seed_key.clone());
        s.bases.insert("marker".into(), vec![seed_key.clone()]);
        s.folders_version = 1;
        s.add_folder("empty").unwrap();
        s.worker = "tester".into(); // A->A mutable access cannot starve a commit.
                                    // Model a publisher/foreground save during preparation, not only dirty
                                    // memory: rebasing must extend this newest journal/durable snapshot.
        s.save(&observer.root).unwrap();
        count.fetch_add(1, Ordering::SeqCst);
    });
    let _hook = Hook;
    d.commit_uploaded(&first, Some(content(b"first"))).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(d.visible().unwrap().file("first").is_some());
    projection_hook::clear();
    let loaded = Namespace::load(root.path(), "tester").unwrap();
    assert_eq!(loaded.pending[0].id, other.id);
    assert_eq!(loaded.pending[0].size, 6);
    assert!(loaded.published.contains(&seed));
    assert_eq!(loaded.bases["marker"], [seed]);
    assert!(loaded.directories.contains("empty"));
    assert!(!loaded.folder_records.is_empty());
    assert_eq!(loaded.folders_version, 1);
    assert!(loaded.committed_intents.contains_key(&first.id));
}

#[test]
fn commit_retry_budget_never_falls_back_to_projection_under_lock() {
    let (_root, d) = drive();
    let intent = write(&d, "first", b"first");
    let observer = d.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    projection_hook::set(move || {
        let mut s = observer
            .state
            .try_lock()
            .expect("retry fallback holds StateLock");
        s.worker = format!("changed-{}", count.fetch_add(1, Ordering::SeqCst));
    });
    let _hook = Hook;
    assert!(d.commit_uploaded(&intent, Some(content(b"first"))).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    let state = d.state.lock().unwrap();
    assert_eq!(state.pending, [intent]);
    assert!(state.events.is_empty());
}

#[test]
fn same_key_event_mutation_and_version_switch_invalidate_caches() {
    let (_root, d) = drive();
    let id = insert(&d, event("file", b"old", vec![], "old"));
    d.visible().unwrap();
    let original = d.state.lock().unwrap().events[&id].clone();
    d.state.lock().unwrap().events.get_mut(&id).unwrap().path = "changed".into();
    assert!(
        d.visible().is_err(),
        "same keys must not hide invalid event identity"
    );
    assert!(d.pool_status().is_err());
    d.state.lock().unwrap().events.insert(id.clone(), original);
    let base = id;
    insert(&d, event("file", b"one", vec![base.clone()], "one"));
    insert(&d, event("file", b"two", vec![base], "two"));
    let v6 = d.visible().unwrap();
    assert!(v6.file("file").is_some());
    d.state.lock().unwrap().version = 3;
    assert_ne!(
        d.visible().unwrap().file("file").unwrap().0,
        v6.file("file").unwrap().0
    );
    assert!(d.pool_status().unwrap().conflicts.is_empty());
}

#[test]
fn stale_uploads_and_mismatched_content_preserve_pending_and_spool() {
    for case in 0..12 {
        let (root, d) = drive();
        let intent = write(&d, "file", b"bytes");
        let mut uploaded = Some(content(b"bytes"));
        {
            let mut s = d.state.lock().unwrap();
            match case {
                0 => s.pending.clear(),
                1 => s.pending[0].id = "aa".repeat(32),
                2 => s.pending[0].path = "other".into(),
                3 => s.pending[0].event_path = "other".into(),
                4 => s.pending[0].parents.push("aa".repeat(32)),
                5 => s.pending[0].depends_on = Some("aa".repeat(32)),
                6 => s.pending[0].spool = None,
                7 => s.pending[0].size += 1,
                8 => s.pending[0].hash = "aa".repeat(32),
                9 => uploaded = None,
                10 => uploaded.as_mut().unwrap().size += 1,
                11 => uploaded.as_mut().unwrap().hash = "aa".repeat(32),
                _ => unreachable!(),
            }
        }
        let before = serde_json::to_vec(&*d.state.lock().unwrap()).unwrap();
        assert!(d.commit_uploaded(&intent, uploaded).is_err(), "case {case}");
        assert_eq!(
            serde_json::to_vec(&*d.state.lock().unwrap()).unwrap(),
            before
        );
        assert_eq!(fs::read(d.spool_path(&intent)).unwrap(), b"bytes");
        assert!(root.path().join("namespace.json").exists());
    }
}

#[test]
fn prepared_commit_save_failure_keeps_live_state_and_replays_safely() {
    for point in [
        "durable.before_persist",
        "namespace.after_previous",
        "namespace.after_journal_append",
        "namespace.journal_torn",
    ] {
        let (root, d) = drive();
        // Enlarge the checkpoint so the third case takes the journal path.
        for n in 0..20 {
            insert(&d, event(&format!("seed-{n}"), b"seed", vec![], "seed"));
        }
        d.state.lock().unwrap().save(root.path()).unwrap();
        let intent = write(&d, "file", b"bytes");
        if matches!(point, "durable.before_persist" | "namespace.after_previous") {
            // A no-op save safely compacts acknowledged intents before forcing
            // the commit to compact; never delete their authoritative journal.
            d.state.lock().unwrap().save(root.path()).unwrap();
            // Change only the checkpoint identity, keeping its valid payload.
            // The original writer detects this and takes its checkpoint path.
            let mut external = Namespace::load(root.path(), "tester").unwrap();
            external.save(root.path()).unwrap();
        }
        let before = serde_json::to_vec(&*d.state.lock().unwrap()).unwrap();
        crate::mount::crash::arm(point);
        let result = d.commit_uploaded(&intent, Some(content(b"bytes")));
        let armed = crate::mount::crash::disarm();
        assert!(result.is_err(), "{point}: {result:?}");
        assert!(armed.is_none(), "failure point not reached: {point}");
        assert_eq!(
            serde_json::to_vec(&*d.state.lock().unwrap()).unwrap(),
            before
        );
        assert_eq!(fs::read(d.spool_path(&intent)).unwrap(), b"bytes");
        let loaded = Namespace::load(root.path(), "tester").unwrap();
        if loaded.committed_intents.contains_key(&intent.id) {
            assert!(loaded.pending.iter().all(|i| i.id != intent.id));
        } else {
            assert!(loaded.pending.iter().any(|i| i == &intent));
        }
        d.commit_uploaded(&intent, Some(content(b"bytes"))).unwrap();
        assert!(Namespace::load(root.path(), "tester")
            .unwrap()
            .committed_intents
            .contains_key(&intent.id));
    }
}

#[test]
fn visible_projection_retries_changed_events_without_locked_fallback() {
    let (_root, d) = drive();
    insert(&d, event("first", b"first", vec![], "first"));
    let observer = d.clone();
    let mut once = true;
    projection_hook::set(move || {
        assert!(observer.state.try_lock().is_some());
        if once {
            once = false;
            insert(&observer, event("later", b"later", vec![], "later"));
        }
    });
    let _hook = Hook;
    let visible = d.visible().unwrap();
    assert!(visible.file("first").is_some());
    assert!(visible.file("later").is_some());
}

#[test]
fn stale_completion_during_preparation_does_not_publish_or_repoint_pin() {
    let (root, d) = drive();
    let intent = write(&d, "file", b"bytes");
    let pin = d.visible_revision("file").unwrap().unwrap();
    d.pins.lock().unwrap().insert("file".into(), pin);
    let before = Namespace::load(root.path(), "tester").unwrap().generation;
    let observer = d.clone();
    projection_hook::set(move || {
        observer.state.try_lock().unwrap().pending[0].path = "replacement".into();
    });
    let _hook = Hook;
    assert!(d.commit_uploaded(&intent, Some(content(b"bytes"))).is_err());
    let s = d.state.lock().unwrap();
    assert!(s.events.is_empty());
    assert_eq!(s.pending[0].path, "replacement");
    assert_eq!(d.pins.lock().unwrap()["file"].id(), intent.id);
    projection_hook::clear();
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().generation,
        before
    );
    assert_eq!(fs::read(d.spool_path(&intent)).unwrap(), b"bytes");
}

#[test]
fn failed_ingest_candidate_and_late_snapshot_cannot_evict_live_projection() {
    let (root, d) = drive();
    let seed = event("seed", b"seed", vec![], "seed");
    {
        let mut s = d.state.lock().unwrap();
        s.insert_event(seed.id().unwrap(), seed);
        s.save(root.path()).unwrap();
    }
    let old = d.state.lock().unwrap().clone();
    let original = old.projected().unwrap();
    let incoming = event("incoming", b"incoming", vec![], "incoming");
    let mut failed = old.clone();
    failed
        .ingest(BTreeMap::from([(incoming.id().unwrap(), incoming.clone())]))
        .unwrap();
    failed.projected().unwrap();
    assert!(failed.save(&root.path().join("absent-directory")).is_err());
    assert!(
        Arc::ptr_eq(&original, &d.state.lock().unwrap().projected().unwrap()),
        "failed ingest polluted live cache"
    );
    {
        let mut s = d.state.lock().unwrap();
        s.ingest(BTreeMap::from([(incoming.id().unwrap(), incoming)]))
            .unwrap();
        s.save(root.path()).unwrap();
    }
    let warm = d.state.lock().unwrap().projected().unwrap();
    old.projected().unwrap(); // Arbitrarily late old status snapshot.
    let observer = d.clone();
    projection_hook::set(move || {
        assert!(
            observer.state.try_lock().is_some(),
            "late snapshot evicted live cache before foreground save"
        );
        panic!("warm live namespace unexpectedly reprojected");
    });
    let _hook = Hook;
    let mut live = d.state.lock().unwrap();
    assert!(Arc::ptr_eq(&warm, &live.projected().unwrap()));
    live.save(root.path()).unwrap();
}

#[test]
fn failed_import_and_late_snapshot_keep_live_projection_warm() {
    for fail in [false, true] {
        let (root, d) = drive();
        d.state.lock().unwrap().save(root.path()).unwrap(); // fresh journal
        let old = d.state.lock().unwrap().clone();
        let original = old.projected().unwrap();
        let manifest = content(b"imported").manifest;
        let path = root.path().join("import.rpool.json");
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        if fail {
            crate::mount::crash::arm("durable.before_persist");
        }
        let result = d.import(path.to_str().unwrap());
        let armed = crate::mount::crash::disarm();
        assert_eq!(result.is_err(), fail);
        if fail {
            assert!(armed.is_none());
        }
        let warm = d.state.lock().unwrap().projected().unwrap();
        if fail {
            assert!(Arc::ptr_eq(&warm, &original));
        }
        old.projected().unwrap();
        let observer = d.clone();
        projection_hook::set(move || {
            assert!(
                observer.state.try_lock().is_some(),
                "import snapshot evicted live projection"
            );
            panic!("warm live namespace unexpectedly reprojected");
        });
        let _hook = Hook;
        let mut live = d.state.lock().unwrap();
        assert!(Arc::ptr_eq(&warm, &live.projected().unwrap()));
        live.save(root.path()).unwrap();
        projection_hook::clear();
    }
}

#[test]
fn rebase_rechecks_worker_device_version_and_dependency_receipt() {
    for case in 0..4 {
        let (_root, d) = drive();
        let parent = write(&d, "file", b"parent");
        d.commit_uploaded(&parent, Some(content(b"parent")))
            .unwrap();
        let mut intent = write(&d, "file", b"child");
        intent.depends_on = Some(parent.id.clone());
        d.state
            .lock()
            .unwrap()
            .pending
            .last_mut()
            .unwrap()
            .depends_on = intent.depends_on.clone();
        let observer = d.clone();
        let dependency = parent.id.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        projection_hook::set(move || {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut s = observer.state.try_lock().unwrap();
                match case {
                    0 => s.worker = "new-worker".into(),
                    1 => s.device = "new-device".into(),
                    2 => s.version = 3,
                    3 => {
                        s.committed_intents.remove(&dependency);
                    }
                    _ => unreachable!(),
                }
            }
        });
        let _hook = Hook;
        let result = d.commit_uploaded(&intent, Some(content(b"child")));
        projection_hook::clear();
        if case == 3 {
            assert!(
                result.is_err(),
                "missing dependency cannot reuse prepared event"
            );
            assert!(d
                .state
                .lock()
                .unwrap()
                .pending
                .iter()
                .any(|i| i.id == intent.id));
        } else {
            result.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            let s = d.state.lock().unwrap();
            let event = &s.events[&s.committed_intents[&intent.id]];
            if case == 0 {
                assert_eq!(event.worker, "new-worker");
            }
            if case == 1 {
                assert_eq!(event.device, "new-device");
            }
            if case == 2 {
                assert_eq!(s.version, 3);
            }
        }
    }
}
