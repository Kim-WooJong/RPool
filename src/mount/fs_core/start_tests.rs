//! The first write into an existing file copies (or clones) its baseline with
//! no namespace or generation lock held, so other files keep working while a
//! large file starts its generation. A test hook stands in for a slow copy by
//! pausing just before the baseline is copied.
use super::clone::no_clone;
use super::generation::baseline_hook;
use super::*;
use crate::mount::virtual_drive::fixture;
use crate::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};
const EDIT: Access = Access::Write {
    truncate: false,
    append: false,
};
const WAIT: Duration = Duration::from_secs(20);
const BIG: usize = 3 << 20;

fn core(root: &Path) -> FsCore {
    FsCore::new(Arc::new(fixture(root))).unwrap()
}
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}
fn put(core: &FsCore, path: &str, bytes: &[u8]) {
    let handle = core.open(path, TRUNC, true, false).unwrap();
    core.write_at(handle, 0, bytes).unwrap();
    core.release(handle).unwrap();
}
fn content(core: &FsCore, path: &str) -> Vec<u8> {
    let handle = core.open(path, Access::Read, false, false).unwrap();
    let bytes = core.read_at(handle, 0, 8 << 20).unwrap();
    core.release(handle).unwrap();
    bytes
}
/// Counts baseline starts of `path` on this thread; the first one signals
/// `ready` and waits for `go` (dropping `go` also releases it).
fn pausing_hook(
    path: &'static str,
    ready: mpsc::Sender<()>,
    go: mpsc::Receiver<()>,
) -> Rc<Cell<usize>> {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    baseline_hook::set(move |started| {
        if started != path {
            return;
        }
        seen.set(seen.get() + 1);
        if seen.get() == 1 {
            let _ = ready.send(());
            let _ = go.recv();
        }
    });
    count
}

fn other_files_work_while_a_baseline_copies(copy: bool) {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let original = pattern(BIG);
    put(&core, "big", &original);
    put(&core, "neighbour", b"old");
    let writer = core.open("big", EDIT, false, false).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core = &core;
        let first = scope.spawn(move || {
            no_clone::set(copy);
            let starts = pausing_hook("big", ready_tx, go_rx);
            let written = core.write_at(writer, 1 << 20, b"NEW");
            baseline_hook::clear();
            no_clone::set(false);
            (written, starts.get())
        });
        ready_rx
            .recv_timeout(WAIT)
            .expect("the write reached its baseline copy");

        // On a worker, so a regression fails the timeout instead of hanging.
        let (done_tx, done_rx) = mpsc::channel();
        scope.spawn(move || {
            // The write has not happened yet: the file still shows its revision.
            let big = core.lookup("big").unwrap();
            assert_eq!(big.size, BIG as u64);
            let fresh = core.open("fresh", TRUNC, true, false).unwrap();
            core.write_at(fresh, 0, b"small").unwrap();
            let fresh_size = core.stat(fresh).unwrap().size;
            core.release(fresh).unwrap();
            let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
            // Exclusive namespace changes of other files proceed too.
            core.mkdir("dir").unwrap();
            core.rename("fresh", "dir/fresh").unwrap();
            core.delete("neighbour").unwrap();
            let moved = content(core, "dir/fresh");
            done_tx.send((fresh_size, names, moved)).unwrap();
        });
        let finished = done_rx.recv_timeout(WAIT);
        // Let the copy finish before any assertion can fail, so no thread hangs.
        drop(go_tx);
        let (fresh_size, names, moved) =
            finished.expect("other files' operations blocked by a baseline copy");
        assert_eq!(fresh_size, 5);
        assert_eq!(names, ["big", "fresh", "neighbour"]);
        assert_eq!(moved, b"small");

        let (written, starts) = first.join().unwrap();
        assert_eq!(written.unwrap(), 3);
        assert_eq!(starts, 1, "nothing raced the copy, so it was installed");
    });
    core.release(writer).unwrap();
    let mut expected = original;
    expected[1 << 20..(1 << 20) + 3].copy_from_slice(b"NEW");
    assert_eq!(content(&core, "big"), expected);
    assert!(matches!(core.lookup("neighbour"), Err(FsError::NotFound)));
}

#[test]
fn other_files_work_while_a_large_baseline_is_copied() {
    other_files_work_while_a_baseline_copies(true);
}

#[test]
fn other_files_work_while_a_large_baseline_is_cloned() {
    other_files_work_while_a_baseline_copies(false);
}

#[test]
fn a_rename_during_the_unlocked_copy_restarts_it_at_the_new_path() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let original = pattern(BIG);
    put(&core, "big", &original);
    let writer = core.open("big", EDIT, false, false).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core = &core;
        let first = scope.spawn(move || {
            let count = Rc::new(Cell::new(0));
            let seen = count.clone();
            baseline_hook::set(move |_| {
                seen.set(seen.get() + 1);
                if seen.get() == 1 {
                    let _ = ready_tx.send(());
                    let _ = go_rx.recv();
                }
            });
            let written = core.write_at(writer, 0, b"HEAD");
            baseline_hook::clear();
            (written, count.get())
        });
        ready_rx
            .recv_timeout(WAIT)
            .expect("the write reached its baseline copy");
        let renamed = core.rename("big", "moved");
        drop(go_tx);
        renamed.unwrap();
        let (written, starts) = first.join().unwrap();
        written.unwrap();
        assert_eq!(starts, 2, "the stale copy was discarded and redone");
    });
    core.release(writer).unwrap();
    let mut expected = original;
    expected[..4].copy_from_slice(b"HEAD");
    assert_eq!(content(&core, "moved"), expected);
    assert!(matches!(core.lookup("big"), Err(FsError::NotFound)));
    // Only the sealed revisions' spools remain: the discarded copy is gone.
    let state = core.drive.state.lock().unwrap();
    for entry in fs::read_dir(root.path().join("spool")).unwrap() {
        let id = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            state.pending.iter().any(|i| i.id == id),
            "leftover spool {id}"
        );
    }
}

#[test]
fn a_delete_during_the_unlocked_copy_fails_the_write_as_stale() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "big", &pattern(BIG));
    let writer = core.open("big", EDIT, false, false).unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let core = &core;
        let first = scope.spawn(move || {
            let _starts = pausing_hook("big", ready_tx, go_rx);
            let written = core.write_at(writer, 0, b"x");
            baseline_hook::clear();
            written
        });
        ready_rx
            .recv_timeout(WAIT)
            .expect("the write reached its baseline copy");
        let deleted = core.delete("big");
        drop(go_tx);
        deleted.unwrap();
        assert!(matches!(first.join().unwrap(), Err(FsError::Stale)));
    });
    core.release(writer).unwrap();
    assert!(matches!(core.lookup("big"), Err(FsError::NotFound)));
}

#[test]
fn truncating_an_existing_file_keeps_the_right_prefix() {
    for copy in [true, false] {
        no_clone::set(copy);
        let root = tempfile::tempdir().unwrap();
        let core = core(root.path());
        let original = pattern(BIG);
        for (path, len) in [("shrink", 1000u64), ("grow", BIG as u64 + 10), ("empty", 0)] {
            put(&core, path, &original);
            let handle = core.open(path, EDIT, false, false).unwrap();
            core.truncate(handle, len).unwrap();
            assert_eq!(core.stat(handle).unwrap().size, len);
            core.write_at(handle, 2, b"ab").unwrap();
            core.release(handle).unwrap();
            let mut expected = original.clone();
            expected.resize(len as usize, 0);
            expected.resize(expected.len().max(4), 0);
            expected[2..4].copy_from_slice(b"ab");
            assert_eq!(content(&core, path), expected, "{path}, copy {copy}");
        }
        // The sealed source images were never changed by their clones.
        put(&core, "source", &original);
        let handle = core.open("source", EDIT, false, false).unwrap();
        core.write_at(handle, 0, b"zz").unwrap();
        let state = core.drive.state.lock().unwrap();
        let sealed = state
            .pending
            .iter()
            .rev()
            .find(|i| i.path == "source")
            .unwrap();
        let image = fs::read(core.drive.spool_path(sealed)).unwrap();
        drop(state);
        assert_eq!(image, original, "copy {copy}");
        core.release(handle).unwrap();
    }
    no_clone::set(false);
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_clones_a_sealed_image_without_touching_it() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let source = root.path().join("source");
    fs::write(&source, pattern(BIG)).unwrap();
    let image = root.path().join("spool").join("image");
    fs::create_dir_all(&image).unwrap();
    let target = image.join("content");
    let mut file = super::clone::clone_image(&drive, &source, &target, BIG as u64)
        .unwrap()
        .expect("APFS temp directories support clonefile");
    file.write_all(b"changed").unwrap();
    drop(file);
    assert_eq!(fs::read(&source).unwrap(), pattern(BIG));
    assert_eq!(&fs::read(&target).unwrap()[..7], b"changed");
}
