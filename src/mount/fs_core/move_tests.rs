//! Renames of saved-but-not-uploaded files through the filesystem core: the
//! sealed spool image moves by link, without being copied or hashed.
use super::*;
use crate::mount::virtual_drive::{fixture, move_hooks};
use crate::prelude::*;
use std::{cell::Cell, rc::Rc};

const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

fn put(core: &FsCore, path: &str, bytes: &[u8]) {
    let handle = core.open(path, TRUNC, true, false).unwrap();
    core.write_at(handle, 0, bytes).unwrap();
    core.release(handle).unwrap();
}
fn content(core: &FsCore, path: &str) -> Vec<u8> {
    let handle = core.open(path, Access::Read, false, false).unwrap();
    let bytes = core.read_at(handle, 0, 1 << 20).unwrap();
    core.release(handle).unwrap();
    bytes
}
/// Seal hashes and MOVE copies made on this thread from now on.
fn reads() -> impl Fn() -> (usize, usize) {
    let hashes = Rc::new(Cell::new(0));
    let counter = hashes.clone();
    crate::mount::crash::hash_hook::set(move || counter.set(counter.get() + 1));
    move_hooks::COPIES.with(|c| c.set(0));
    move || (hashes.get(), move_hooks::COPIES.with(|c| c.get()))
}
fn assert_sealed_images_intact(core: &FsCore) {
    for intent in core.drive.state.lock().unwrap().pending.iter() {
        if intent.spool.is_some() {
            let path = core.drive.spool_path(intent);
            assert_eq!(
                crate::utils::hash_file_range(&path, 0, intent.size).unwrap(),
                intent.hash
            );
        }
    }
}
/// The committed revision at `path` carries the hash of `bytes`.
fn assert_committed(core: &FsCore, path: &str, bytes: &[u8]) {
    let hash = blake3::hash(bytes).to_hex().to_string();
    match core.drive.visible_revision(path).unwrap().unwrap() {
        crate::mount::virtual_drive::Revision::Cloud { content, .. } => {
            assert_eq!((content.hash, content.size), (hash, bytes.len() as u64))
        }
        _ => panic!("{path} is committed"),
    }
}
fn no_staging_left(root: &Path) {
    for entry in fs::read_dir(root.join("spool")).unwrap() {
        let moving = entry.unwrap().path().join("moving");
        if moving.exists() {
            assert_eq!(fs::read_dir(moving).unwrap().count(), 0);
        }
    }
}

#[test]
fn save_to_temp_then_rename_moves_without_reading() {
    let root = tempfile::tempdir().unwrap();
    let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
    put(&core, "doc.tmp", b"large document");
    let counted = reads();
    core.rename("doc.tmp", "doc").unwrap();
    assert_eq!(counted(), (0, 0), "no hash, no copy");
    crate::mount::crash::hash_hook::clear();
    assert_eq!(content(&core, "doc"), b"large document");
    assert!(core.lookup("doc.tmp").is_err());
    assert_sealed_images_intact(&core);
    assert_eq!(core.drive.spool_bytes().unwrap(), 14);
    no_staging_left(root.path());
    super::tests::commit_all(&core.drive);
    assert_committed(&core, "doc", b"large document");
}

#[test]
fn folder_rename_moves_pending_files_without_reading() {
    let root = tempfile::tempdir().unwrap();
    let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
    put(&core, "dir/a", b"alpha");
    put(&core, "dir/sub/b", b"beta");
    let counted = reads();
    core.rename("dir", "moved").unwrap();
    assert_eq!(counted(), (0, 0));
    crate::mount::crash::hash_hook::clear();
    assert_eq!(content(&core, "moved/a"), b"alpha");
    assert_eq!(content(&core, "moved/sub/b"), b"beta");
    assert_sealed_images_intact(&core);
    assert_eq!(core.drive.spool_bytes().unwrap(), 9);
    no_staging_left(root.path());
    super::tests::commit_all(&core.drive);
    assert_committed(&core, "moved/a", b"alpha");
    assert_committed(&core, "moved/sub/b", b"beta");
}

#[test]
fn an_open_unsealed_file_is_sealed_then_moved() {
    let root = tempfile::tempdir().unwrap();
    let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
    put(&core, "a", b"old");
    let handle = core.open("a", TRUNC, false, false).unwrap();
    core.write_at(handle, 0, b"new bytes").unwrap();
    core.rename("a", "b").unwrap();
    core.release(handle).unwrap();
    assert_eq!(content(&core, "b"), b"new bytes");
    assert_sealed_images_intact(&core);
    no_staging_left(root.path());
}

#[test]
fn without_hard_links_the_copy_is_staged_before_the_lock() {
    let root = tempfile::tempdir().unwrap();
    let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
    put(&core, "dir/a", b"alpha");
    move_hooks::NO_LINK.with(|n| n.set(true));
    let counted = reads();
    core.rename("dir", "moved").unwrap();
    move_hooks::NO_LINK.with(|n| n.set(false));
    assert_eq!(counted(), (0, 1), "one copy, hashed while copying");
    crate::mount::crash::hash_hook::clear();
    assert_eq!(content(&core, "moved/a"), b"alpha");
    assert_sealed_images_intact(&core);
    assert_eq!(core.drive.spool_bytes().unwrap(), 10);
    no_staging_left(root.path());
}
