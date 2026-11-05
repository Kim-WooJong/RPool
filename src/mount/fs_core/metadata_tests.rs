//! Save counts and read ancestry across failure/retry, without timing gates.
use super::*;
use crate::mount::crash;
use crate::mount::namespace::Namespace;
use crate::mount::shared_model::Event;
use crate::mount::virtual_drive::fixture;
use crate::mount::virtual_tests::content;
use std::sync::mpsc;

fn peer(root: &Path) -> (FsCore, String) {
    let mut drive = fixture(root);
    drive.pool_sync_roots = vec!["unused:metadata".into()];
    let event = Event {
        version: 1,
        device: "peer".into(),
        worker: "peer".into(),
        path: "doc".into(),
        parents: vec![],
        content: Some(content(b"")),
    };
    let id = event.id().unwrap();
    {
        let mut s = drive.state.lock().unwrap();
        s.version = 6;
        s.events.insert(id.clone(), event);
        s.save(root).unwrap();
    }
    (FsCore::new(Arc::new(drive)).unwrap(), id)
}

#[test]
fn failed_read_ancestry_save_is_retried_not_cached_as_success() {
    let root = tempfile::tempdir().unwrap();
    let (core, id) = peer(root.path());
    let handle = core.open("doc", Access::Read, false, false).unwrap();
    crash::arm("durable.before_persist");
    assert!(core.read_at(handle, 0, 1).is_err());
    crash::disarm();
    assert!(core.observed.lock().unwrap().is_empty());
    assert!(!core.drive.state.lock().unwrap().bases.contains_key("doc"));
    assert!(core.read_at(handle, 0, 1).unwrap().is_empty());
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().bases["doc"],
        [id]
    );
    let generation = core.drive.state.lock().unwrap().generation;
    let slow = slow_paths(|| {
        for _ in 0..32 {
            core.read_at(handle, 0, 1).unwrap();
        }
    });
    assert_eq!(slow, 0, "a recorded revision is not observed again");
    assert_eq!(core.drive.state.lock().unwrap().generation, generation);
    core.release(handle).unwrap();
}

#[test]
fn alternating_snapshot_reads_persist_the_revision_actually_read() {
    let root = tempfile::tempdir().unwrap();
    let (core, first) = peer(root.path());
    let old = core.open("doc", Access::Read, false, false).unwrap();
    core.read_at(old, 0, 1).unwrap();
    let event = Event {
        version: 1,
        device: "peer".into(),
        worker: "peer".into(),
        path: "doc".into(),
        parents: vec![first.clone()],
        content: Some(content(b"")),
    };
    let next = event.id().unwrap();
    core.drive
        .state
        .lock()
        .unwrap()
        .events
        .insert(next.clone(), event);
    let new = core.open("doc", Access::Read, false, false).unwrap();
    for (handle, id) in [(new, next.clone()), (old, first.clone()), (new, next)] {
        core.read_at(handle, 0, 1).unwrap();
        assert_eq!(
            Namespace::load(root.path(), "tester").unwrap().bases["doc"],
            [id]
        );
    }
    core.release(new).unwrap();
    core.release(old).unwrap();
}

#[test]
fn repeated_native_saves_only_persist_the_initial_base_and_each_seal() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.pool_sync_roots = vec!["unused:metadata".into()];
    drive.state.lock().unwrap().version = 6;
    let core = FsCore::new(Arc::new(drive)).unwrap();
    let before = core.drive.state.lock().unwrap().generation;
    for round in 0..12 {
        let handle = core
            .open(
                "doc",
                Access::Write {
                    truncate: true,
                    append: false,
                },
                true,
                false,
            )
            .unwrap();
        core.write_at(handle, 0, &[round]).unwrap();
        core.fsync(handle).unwrap();
        core.release(handle).unwrap();
    }
    let state = core.drive.state.lock().unwrap();
    assert_eq!(
        state.generation - before,
        13,
        "one base save plus twelve seals"
    );
    assert_eq!(state.pending.len(), 12);
    for pair in state.pending.windows(2) {
        assert_eq!(pair[1].depends_on.as_deref(), Some(pair[0].id.as_str()));
    }
    let loaded = Namespace::load(root.path(), "tester").unwrap();
    assert_eq!(loaded.pending, state.pending);
}

/// How often `f` entered `remember`'s slow path on this thread.
fn slow_paths(f: impl FnOnce()) -> usize {
    let count = Arc::new(Mutex::new(0));
    let counted = count.clone();
    observe_hook::set(move |stage| {
        if stage == "observing" {
            *counted.lock().unwrap() += 1;
        }
    });
    f();
    observe_hook::clear();
    let slow = *count.lock().unwrap();
    slow
}

/// Second cloud revision of `doc` descending from `first` (a peer's edit).
fn next_revision(core: &FsCore, first: &str) -> String {
    let event = Event {
        version: 1,
        device: "peer".into(),
        worker: "peer".into(),
        path: "doc".into(),
        parents: vec![first.into()],
        content: Some(content(b"")),
    };
    let next = event.id().unwrap();
    core.drive
        .state
        .lock()
        .unwrap()
        .events
        .insert(next.clone(), event);
    next
}

/// A repeated read of a recorded revision never touches the drive's
/// namespace lock: it completes while another thread holds that lock.
#[test]
fn repeated_reads_do_not_wait_for_the_namespace_lock() {
    let root = tempfile::tempdir().unwrap();
    let (core, _) = peer(root.path());
    let core = Arc::new(core);
    let handle = core.open("doc", Access::Read, false, false).unwrap();
    core.read_at(handle, 0, 1).unwrap();
    let held = core.drive.state.lock().unwrap();
    let (done, finished) = mpsc::channel();
    let reader = {
        let core = core.clone();
        std::thread::spawn(move || {
            let slow = slow_paths(|| {
                for _ in 0..32 {
                    core.read_at(handle, 0, 1).unwrap();
                }
            });
            done.send(slow).unwrap();
        })
    };
    // Not a speed bound: a read that needed the held lock could never finish,
    // and the timeout only turns that deadlock into a failure.
    let slow = finished
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("a repeated read waited for the namespace lock");
    drop(held);
    reader.join().unwrap();
    assert_eq!(slow, 0);
    core.release(handle).unwrap();
}

/// Two handles observe different revisions at once. While the first is
/// between its save and its publication, the second cannot save and publish
/// in between, so `observed` always names the durable base, and the next
/// read of the revision that lost still records it.
#[test]
fn concurrent_changed_observations_publish_in_save_order() {
    let root = tempfile::tempdir().unwrap();
    let (core, first) = peer(root.path());
    let core = Arc::new(core);
    let old = core.open("doc", Access::Read, false, false).unwrap();
    let second = next_revision(&core, &first);
    let new = core.open("doc", Access::Read, false, false).unwrap();
    let file = core.ids.lock().unwrap().get("doc").unwrap();
    let (saved, saved_rx) = mpsc::channel();
    let (go, go_rx) = mpsc::channel::<()>();
    let a = {
        let core = core.clone();
        std::thread::spawn(move || {
            observe_hook::set(move |stage| {
                if stage == "saved" {
                    saved.send(()).unwrap();
                    go_rx.recv().unwrap();
                }
            });
            core.read_at(old, 0, 1).unwrap();
            observe_hook::clear();
        })
    };
    saved_rx.recv().unwrap();
    // A has saved `first` and not yet published it.
    let (done, done_rx) = mpsc::channel();
    let b = {
        let core = core.clone();
        std::thread::spawn(move || {
            core.read_at(new, 0, 1).unwrap();
            done.send(()).unwrap();
        })
    };
    // Unserialized, B would finish here (saving and publishing `second`)
    // before A publishes `first`. Serialized, B waits for A; the wait only
    // decides whether that interleaving was offered, never the outcome.
    let _ = done_rx.recv_timeout(std::time::Duration::from_millis(200));
    go.send(()).unwrap();
    a.join().unwrap();
    b.join().unwrap();
    let durable = |root: &Path| Namespace::load(root, "tester").unwrap().bases["doc"].clone();
    let observed = core.observed.lock().unwrap()[&file].id().to_string();
    assert_eq!(observed, second, "B observed after A published");
    assert_eq!(durable(root.path()), [second.as_str()]);
    assert_eq!(core.drive.state.lock().unwrap().bases["doc"], [second]);
    // The revision that was overtaken is recorded again when read again.
    core.read_at(old, 0, 1).unwrap();
    assert_eq!(durable(root.path()), [first]);
    core.release(new).unwrap();
    core.release(old).unwrap();
}
