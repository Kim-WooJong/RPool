//! Seals of large files hash with no lock held (`prehash`), so the rest of
//! the drive keeps working while one file is being sealed. A test hook stands
//! in for a slow hash by pausing between the spool fsync and its hash.
use super::*;
use crate::mount::crash::hash_hook;
use crate::mount::virtual_drive::{fixture, VirtualDrive};
use crate::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};
const WAIT: Duration = Duration::from_secs(20);

fn core(root: &Path) -> FsCore {
    FsCore::new(Arc::new(fixture(root))).unwrap()
}
fn content(core: &FsCore, path: &str) -> Vec<u8> {
    let handle = core.open(path, Access::Read, false, false).unwrap();
    let bytes = core.read_at(handle, 0, 4 << 20).unwrap();
    core.release(handle).unwrap();
    bytes
}
fn sealed_paths(drive: &VirtualDrive) -> Vec<String> {
    let state = drive.state.lock().unwrap();
    for intent in state.pending.iter().filter(|i| i.spool.is_some()) {
        let image = drive.spool_path(intent);
        assert_eq!(
            crate::utils::hash_file_range(&image, 0, intent.size).unwrap(),
            intent.hash,
            "sealed image of {} matches its recorded hash",
            intent.path
        );
        assert_eq!(fs::metadata(&image).unwrap().len(), intent.size);
    }
    state.pending.iter().map(|i| i.path.clone()).collect()
}
/// Counts hashes on this thread; the first one signals `ready` and waits for
/// `go` (dropping `go` also releases it).
fn pausing_hook(ready: mpsc::Sender<()>, go: mpsc::Receiver<()>) -> Rc<Cell<usize>> {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    hash_hook::set(move || {
        seen.set(seen.get() + 1);
        if seen.get() == 1 {
            let _ = ready.send(());
            let _ = go.recv();
        }
    });
    count
}

#[test]
fn other_files_can_be_created_listed_and_moved_while_a_large_seal_hashes() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let big = core.open("big", TRUNC, true, false).unwrap();
    core.write_at(big, 0, &vec![7u8; 3 << 20]).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core = &core;
        let sealer = scope.spawn(move || {
            let hashes = pausing_hook(ready_tx, go_rx);
            let released = core.release(big);
            hash_hook::clear();
            (released, hashes.get())
        });
        ready_rx.recv_timeout(WAIT).expect("seal reached its hash");
        // The last close is mid-hash: not acknowledged yet.
        assert!(!sealed_paths(&core.drive).contains(&"big".to_string()));

        // On a worker, so a regression fails the timeout instead of hanging.
        let (done_tx, done_rx) = mpsc::channel();
        scope.spawn(move || {
            assert_eq!(core.lookup("big").unwrap().size, 3 << 20);
            let other = core.open("other", TRUNC, true, false).unwrap();
            core.write_at(other, 0, b"small").unwrap();
            core.release(other).unwrap();
            let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
            core.mkdir("dir").unwrap();
            core.rename("other", "dir/other").unwrap();
            let moved = content(core, "dir/other");
            done_tx.send((names, moved)).unwrap();
        });
        let finished = done_rx.recv_timeout(WAIT);
        // Let the seal finish before any assertion can fail, so no thread hangs.
        drop(go_tx);
        let (names, moved) = finished.expect("namespace operations blocked by a seal's hash");
        assert_eq!(names, vec!["big".to_string(), "other".to_string()]);
        assert_eq!(moved, b"small");

        let (released, hashes) = sealer.join().unwrap();
        released.unwrap();
        assert_eq!(hashes, 1, "the seal reused the hash taken without the lock");
    });
    assert!(sealed_paths(&core.drive).contains(&"big".to_string()));
    assert_eq!(content(&core, "big"), vec![7u8; 3 << 20]);
}

#[test]
fn a_write_during_the_unlocked_hash_is_sealed_with_a_fresh_hash() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let writer = core.open("a", TRUNC, true, false).unwrap();
    core.write_at(writer, 0, b"hello").unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core = &core;
        let sealer = scope.spawn(move || {
            let hashes = pausing_hook(ready_tx, go_rx);
            let synced = core.fsync(writer);
            hash_hook::clear();
            (synced, hashes.get())
        });
        ready_rx.recv_timeout(WAIT).expect("fsync reached its hash");
        let written = core.write_at(writer, 5, b" world");
        drop(go_tx);
        written.unwrap();
        let (synced, hashes) = sealer.join().unwrap();
        synced.unwrap();
        assert_eq!(hashes, 2, "the stale hash was discarded and redone");
    });
    // fsync returned after the racing write, so its seal includes it.
    assert_eq!(content(&core, "a"), b"hello world");
    assert_eq!(sealed_paths(&core.drive), vec!["a".to_string()]);
    core.release(writer).unwrap();
    assert_eq!(sealed_paths(&core.drive), vec!["a".to_string()]);
}

#[test]
fn a_seal_hashes_once_and_a_failed_hash_leaves_the_generation_unsealed() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    hash_hook::set(move || seen.set(seen.get() + 1));
    let writer = core.open("a", TRUNC, true, false).unwrap();
    assert!(core.writes(writer), "a write handle's close may seal");
    core.write_at(writer, 0, b"one").unwrap();
    crate::mount::crash::arm("seal.before_fsync");
    assert!(
        core.fsync(writer).is_err(),
        "a failed flush is a failed seal"
    );
    assert!(sealed_paths(&core.drive).is_empty());
    core.fsync(writer).unwrap();
    assert_eq!(count.get(), 1);
    core.release(writer).unwrap();
    assert_eq!(count.get(), 1, "a clean last close does not hash again");
    hash_hook::clear();
    assert_eq!(sealed_paths(&core.drive), vec!["a".to_string()]);
    assert_eq!(content(&core, "a"), b"one");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    assert!(!core.writes(reader));
    core.release(reader).unwrap();
    assert!(!core.writes(reader), "a closed handle");
}
