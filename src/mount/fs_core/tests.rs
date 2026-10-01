//! Exit-gate traces for the filesystem core on a local fixture workspace.
//! No OS mount, rclone or network. "Crash" drops the core and drive without
//! releasing handles, then reopens the workspace from disk.
use super::*;
use crate::mount::virtual_drive::{fixture, fixture_reopen, VirtualDrive};
use crate::prelude::*;

const W: Access = Access::Write {
    truncate: false,
    append: false,
};
const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

fn core(root: &Path) -> FsCore {
    FsCore::new(Arc::new(fixture(root))).unwrap()
}
fn reopen(root: &Path) -> FsCore {
    FsCore::new(Arc::new(fixture_reopen(root))).unwrap()
}
fn read_all(core: &FsCore, handle: HandleId) -> Vec<u8> {
    core.read_at(handle, 0, 1 << 20).unwrap()
}
fn content(core: &FsCore, path: &str) -> Vec<u8> {
    let handle = core.open(path, Access::Read, false, false).unwrap();
    let bytes = read_all(core, handle);
    core.release(handle).unwrap();
    bytes
}
fn put(core: &FsCore, path: &str, bytes: &[u8]) {
    let handle = core.open(path, TRUNC, true, false).unwrap();
    core.write_at(handle, 0, bytes).unwrap();
    core.release(handle).unwrap();
}
fn pending(drive: &VirtualDrive) -> usize {
    drive.state.lock().unwrap().pending.len()
}
/// Sealed (acknowledged) spool images are never mutated.
fn assert_published_unchanged(drive: &VirtualDrive) {
    for intent in drive.state.lock().unwrap().pending.iter() {
        if intent.spool.is_some() {
            let path = drive.spool_path(intent);
            assert_eq!(
                crate::utils::hash_file_range(&path, 0, intent.size).unwrap(),
                intent.hash
            );
            assert_eq!(fs::metadata(&path).unwrap().len(), intent.size);
        }
    }
}

#[test]
fn fsync_acknowledges_locally_without_any_upload() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let handle = core.open("a", TRUNC, true, false).unwrap();
    core.write_at(handle, 0, b"hello").unwrap();
    assert_eq!(pending(&core.drive), 0, "write_at is volatile");
    core.fsync(handle).unwrap();
    assert_eq!(pending(&core.drive), 1);
    // The uploader (rclone) is unavailable in the fixture; nothing waited on it.
    drop(core);
    let core = reopen(root.path());
    assert_eq!(content(&core, "a"), b"hello");
    assert_published_unchanged(&core.drive);
}

#[test]
fn abrupt_exit_keeps_acknowledged_data_and_drops_unacknowledged_writes() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"acknowledged");
    let handle = core.open("a", W, false, false).unwrap();
    core.write_at(handle, 0, b"UNACKED").unwrap();
    let created = core.open("new", TRUNC, true, false).unwrap();
    core.write_at(created, 0, b"never synced").unwrap();
    assert_eq!(core.lookup("new").unwrap().size, 12);
    drop(core); // no release, no fsync
    let core = reopen(root.path());
    assert_eq!(content(&core, "a"), b"acknowledged");
    assert!(matches!(core.lookup("new"), Err(FsError::NotFound)));
    assert_eq!(pending(&core.drive), 1);
    // Unsealed spools stay on disk for recovery; they are not deleted.
    let spools = fs::read_dir(root.path().join("spool")).unwrap().count();
    assert_eq!(spools, 3);
    assert_published_unchanged(&core.drive);
}

#[test]
fn old_reader_keeps_its_snapshot_across_overwrite_rename_and_delete() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"old bytes");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    put(&core, "a", b"new");
    assert_eq!(read_all(&core, reader), b"old bytes");
    core.rename("a", "b").unwrap();
    assert_eq!(read_all(&core, reader), b"old bytes");
    assert_eq!(content(&core, "b"), b"new");
    core.delete("b").unwrap();
    assert_eq!(core.read_at(reader, 4, 5).unwrap(), b"bytes");
    core.release(reader).unwrap();
    assert_published_unchanged(&core.drive);
}

#[test]
fn concurrent_writers_share_one_generation_and_one_revision() {
    let root = tempfile::tempdir().unwrap();
    let core = Arc::new(core(root.path()));
    put(&core, "a", &[b'.'; 8]);
    let before = pending(&core.drive);
    let first = core.open("a", W, false, false).unwrap();
    let second = core.open("a", W, false, false).unwrap();
    std::thread::scope(|scope| {
        let c = core.clone();
        scope.spawn(move || c.write_at(first, 0, b"AAAA").unwrap());
        let c = core.clone();
        scope.spawn(move || c.write_at(second, 4, b"BBBB").unwrap());
    });
    assert_eq!(
        read_all(&core, first),
        b"AAAABBBB",
        "writers see each other"
    );
    core.fsync(first).unwrap();
    core.release(first).unwrap();
    core.release(second).unwrap();
    assert_eq!(pending(&core.drive), before + 1);
    assert_eq!(content(&core, "a"), b"AAAABBBB");
    assert_published_unchanged(&core.drive);
}

#[test]
fn rename_over_keeps_readers_and_never_acknowledges_the_replaced_writer() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"source");
    put(&core, "b", b"old destination");
    let reader = core.open("b", Access::Read, false, false).unwrap();
    let writer = core.open("b", W, false, false).unwrap();
    core.write_at(writer, 0, b"unacked").unwrap();
    let orphan = core.lookup("b").unwrap().tag;
    core.rename("a", "b").unwrap();
    assert_eq!(read_all(&core, reader), b"old destination");
    assert_eq!(content(&core, "b"), b"source");
    assert!(matches!(core.lookup("a"), Err(FsError::NotFound)));
    assert!(matches!(core.fsync(writer), Err(FsError::Stale)));
    core.release(writer).unwrap();
    assert!(!root.path().join("spool").join(&orphan).exists());
    assert_eq!(content(&core, "b"), b"source");
    assert_published_unchanged(&core.drive);
}

#[test]
fn rename_seals_unsealed_source_writes_first() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let writer = core.open("d/a", TRUNC, true, false).unwrap();
    core.write_at(writer, 0, b"in flight").unwrap();
    core.rename("d", "e").unwrap();
    assert_eq!(content(&core, "e/a"), b"in flight");
    // The handle follows its file; later writes land at the new path.
    core.write_at(writer, 0, b"IN").unwrap();
    core.release(writer).unwrap();
    assert_eq!(content(&core, "e/a"), b"IN flight");
    assert!(matches!(core.lookup("d/a"), Err(FsError::NotFound)));
    assert_published_unchanged(&core.drive);
}

#[test]
fn delete_and_recreate_gets_a_new_identity() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"first");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    let first = core.lookup("a").unwrap().id;
    core.delete("a").unwrap();
    assert!(matches!(core.lookup("a"), Err(FsError::NotFound)));
    assert!(matches!(core.delete("a"), Err(FsError::NotFound)));
    put(&core, "a", b"second");
    assert_ne!(core.lookup("a").unwrap().id, first);
    assert_eq!(content(&core, "a"), b"second");
    assert_eq!(read_all(&core, reader), b"first");
    assert_published_unchanged(&core.drive);
}

#[test]
fn deleting_an_unsealed_create_discards_it_at_last_release() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let writer = core.open("tmp", TRUNC, true, false).unwrap();
    core.write_at(writer, 0, b"scratch").unwrap();
    let spool = core.lookup("tmp").unwrap().tag;
    core.delete("tmp").unwrap();
    assert!(matches!(core.lookup("tmp"), Err(FsError::NotFound)));
    core.write_at(writer, 7, b" more").unwrap();
    assert_eq!(read_all(&core, writer), b"scratch more");
    core.release(writer).unwrap();
    assert!(!root.path().join("spool").join(spool).exists());
    assert_eq!(pending(&core.drive), 0);
}

#[test]
fn retried_fsync_flush_and_release_are_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    let handle = core.open("a", TRUNC, true, false).unwrap();
    core.write_at(handle, 0, b"once").unwrap();
    core.fsync(handle).unwrap();
    core.fsync(handle).unwrap();
    core.flush(handle).unwrap();
    core.freeze(handle).unwrap();
    core.release(handle).unwrap();
    assert!(matches!(core.release(handle), Err(FsError::BadHandle)));
    assert!(matches!(core.fsync(handle), Err(FsError::BadHandle)));
    assert_eq!(pending(&core.drive), 1);
    // Opening for write without writing creates no revision.
    let idle = core.open("a", W, false, false).unwrap();
    core.release(idle).unwrap();
    assert_eq!(pending(&core.drive), 1);
    assert_published_unchanged(&core.drive);
}

#[test]
fn spool_budget_refuses_writes_before_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.spool_limit = 16;
    let core = FsCore::new(Arc::new(drive)).unwrap();
    put(&core, "a", b"12345678");
    let handle = core.open("b", TRUNC, true, false).unwrap();
    assert!(matches!(
        core.write_at(handle, 0, &[0; 9]),
        Err(FsError::NoSpace)
    ));
    assert!(matches!(core.truncate(handle, 9), Err(FsError::NoSpace)));
    core.write_at(handle, 0, b"fits").unwrap();
    core.release(handle).unwrap();
    assert_eq!(content(&core, "a"), b"12345678");
    assert_eq!(content(&core, "b"), b"fits");
    assert_published_unchanged(&core.drive);
}

#[test]
fn truncate_and_sparse_writes_copy_on_write_from_the_sealed_revision() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"abcdef");
    let sealed = core.lookup("a").unwrap().tag;
    let handle = core.open("a", W, false, false).unwrap();
    core.truncate(handle, 3).unwrap();
    core.write_at(handle, 5, b"z").unwrap();
    core.release(handle).unwrap();
    assert_eq!(content(&core, "a"), b"abc\0\0z");
    assert_ne!(core.lookup("a").unwrap().tag, sealed);
    let append = Access::Write {
        truncate: false,
        append: true,
    };
    let handle = core.open("a", append, false, false).unwrap();
    core.write_at(handle, 0, b"!").unwrap();
    core.release(handle).unwrap();
    assert_eq!(content(&core, "a"), b"abc\0\0z!");
    assert_published_unchanged(&core.drive);
}

#[test]
fn open_flags_and_directories_follow_the_overlay() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    assert!(matches!(
        core.open("a", Access::Read, true, false),
        Err(FsError::NotFound)
    ));
    assert!(matches!(
        core.open("", W, true, false),
        Err(FsError::InvalidPath)
    ));
    let writer = core.open("d/a", TRUNC, true, false).unwrap();
    assert!(matches!(
        core.open("d/a", W, true, true),
        Err(FsError::Exists)
    ));
    let names: Vec<String> = core
        .readdir("d")
        .unwrap()
        .into_iter()
        .map(|e| e.0)
        .collect();
    assert_eq!(names, ["a"]);
    assert!(core.lookup("d").unwrap().directory);
    assert!(matches!(core.rmdir("d"), Err(FsError::NotEmpty)));
    assert!(matches!(
        core.open("d", Access::Read, false, false),
        Err(FsError::IsDir)
    ));
    let reader = core.open("d/a", Access::Read, false, false).unwrap();
    core.write_at(writer, 0, b"live").unwrap();
    assert_eq!(
        read_all(&core, reader),
        b"live",
        "opened while unsealed: attached"
    );
    assert!(matches!(
        core.write_at(reader, 0, b"x"),
        Err(FsError::ReadOnly)
    ));
    core.release(writer).unwrap();
    core.release(reader).unwrap();
    core.mkdir("empty").unwrap();
    assert!(matches!(core.mkdir("empty"), Err(FsError::Exists)));
    let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, ["d", "empty"]);
    core.rmdir("empty").unwrap();
    assert_eq!(core.statfs(), core.drive.quota());
    assert!(matches!(core.readdir("d/a"), Err(FsError::NotDir)));
}

/// Commit every pending intent in order, as `sync` does after uploading.
pub(super) fn commit_all(drive: &VirtualDrive) {
    let pending = drive.state.lock().unwrap().pending.clone();
    for intent in pending {
        let content = intent.spool.as_ref().map(|_| {
            let bytes = fs::read(drive.spool_path(&intent)).unwrap();
            crate::mount::virtual_tests::content(&bytes)
        });
        drive.commit_uploaded(&intent, content).unwrap();
    }
}

#[test]
fn successive_generations_commit_as_one_history_without_conflict_copies() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"one");
    let handle = core.open("a", W, false, false).unwrap();
    core.write_at(handle, 0, b"two").unwrap();
    core.freeze(handle).unwrap();
    core.write_at(handle, 3, b"!").unwrap();
    core.release(handle).unwrap();
    put(&core, "a", b"four");
    core.rename("a", "d/b").unwrap();
    put(&core, "d/b", b"five");
    commit_all(&core.drive);
    let view = core.drive.view().unwrap();
    assert_eq!(view.keys().collect::<Vec<_>>(), ["d/b"], "no sibling heads");
    assert_eq!(view["d/b"].size(), 4);
    let state = core.drive.state.lock().unwrap();
    assert!(state.pending.is_empty());
    let head = state.resolved().unwrap()["d/b"].event_id.clone();
    let mut depth = 0;
    let mut cursor = Some(head);
    while let Some(id) = cursor {
        let event = &state.events[&id];
        assert!(event.parents.len() <= 1);
        cursor = event.parents.first().cloned();
        depth += 1;
    }
    assert!(
        depth >= 2,
        "five descends from the moved revision, depth {depth}"
    );
    // The source history is one chain ending in the move's deletion.
    let mut at_a: Vec<_> = state.events.values().filter(|e| e.path == "a").collect();
    at_a.sort_by_key(|e| e.parents.len());
    assert_eq!(at_a.len(), 5, "one, two, two!, four, deletion");
    assert!(at_a.iter().filter(|e| e.parents.is_empty()).count() == 1);
}

#[test]
fn directory_rename_onto_unsealed_creates_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "src/a", b"a");
    let writer = core.open("dst/x", TRUNC, true, false).unwrap();
    core.write_at(writer, 0, b"unsealed").unwrap();
    assert!(matches!(core.rename("src", "dst"), Err(FsError::Exists)));
    core.release(writer).unwrap();
    assert_eq!(content(&core, "dst/x"), b"unsealed");
}

#[test]
fn a_new_write_starts_from_the_latest_seal_even_with_an_old_handle() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"base");
    let stale = core.open("a", W, false, false).unwrap();
    let other = core.open("a", W, false, false).unwrap();
    core.write_at(other, 0, b"NEW!").unwrap();
    core.fsync(other).unwrap();
    // `stale` was opened before that seal; its write must not revert it.
    core.write_at(stale, 4, b"+").unwrap();
    core.release(stale).unwrap();
    core.release(other).unwrap();
    assert_eq!(content(&core, "a"), b"NEW!+");
}

#[test]
fn local_and_pool_sync_workspaces_open() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.pool_sync_roots = vec!["remote:pool".into()];
    assert!(FsCore::new(Arc::new(drive)).is_ok(), "pool sync v6");
    let root = tempfile::tempdir().unwrap();
    assert!(
        FsCore::new(Arc::new(fixture(root.path()))).is_ok(),
        "local fixture"
    );
}

#[test]
fn a_stalled_uploader_never_blocks_acknowledgement_or_namespace_changes() {
    let root = tempfile::tempdir().unwrap();
    let core = Arc::new(core(root.path()));
    put(&core, "a", b"before");
    // `sync` holds this gate for its whole upload; hold it as a stalled upload.
    let stalled = core.drive.sync_gate.lock().unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = core.clone();
    std::thread::spawn(move || {
        let handle = worker.open("a", W, false, false).unwrap();
        worker.write_at(handle, 0, b"AFTER!").unwrap();
        worker.fsync(handle).unwrap();
        worker.release(handle).unwrap();
        put(&worker, "b", b"new");
        worker.rename("b", "c").unwrap();
        worker.delete("c").unwrap();
        worker.mkdir("dir").unwrap();
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("operations waited on the uploader");
    drop(stalled);
    assert_eq!(content(&core, "a"), b"AFTER!");
    assert!(matches!(core.lookup("c"), Err(FsError::NotFound)));
    assert_published_unchanged(&core.drive);
}

#[test]
fn stat_follows_the_handle_through_writes_and_unlink() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "a", b"12345");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    let writer = core.open("a", W, false, false).unwrap();
    core.write_at(writer, 5, b"678").unwrap();
    assert_eq!(core.stat(writer).unwrap().size, 8);
    assert_eq!(core.stat(reader).unwrap().size, 5, "snapshot");
    core.release(writer).unwrap();
    core.delete("a").unwrap();
    assert_eq!(core.stat(reader).unwrap().size, 5);
    assert!(matches!(core.stat(HandleId(9999)), Err(FsError::BadHandle)));
}

#[test]
fn a_failed_final_seal_leaves_no_phantom_file() {
    let root = tempfile::tempdir().unwrap();
    let core = core(root.path());
    put(&core, "twin", b"first");
    // The drive refuses names that differ only in case at seal time.
    let handle = core.open("Twin", TRUNC, true, false).unwrap();
    core.write_at(handle, 0, b"second").unwrap();
    assert!(matches!(core.release(handle), Err(FsError::Io(_))));
    assert!(matches!(core.lookup("Twin"), Err(FsError::NotFound)));
    let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
    assert_eq!(names, ["twin"]);
    assert_eq!(content(&core, "twin"), b"first");
    assert_published_unchanged(&core.drive);
}
