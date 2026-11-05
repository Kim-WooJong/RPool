//! Guards the existing unlocked prehash contract; WinFsp's outer callback
//! guard is separate and is deliberately not changed by the core.
use super::*;
use crate::mount::crash::hash_hook;
use crate::mount::fs_core::Access;
use crate::mount::virtual_drive::fixture;
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn rename_hashes_before_its_exclusive_core_namespace_section() {
    let root = tempfile::tempdir().unwrap();
    let core = Arc::new(FsCore::new(Arc::new(fixture(root.path()))).unwrap());
    let handle = core
        .open(
            "draft",
            Access::Write {
                truncate: true,
                append: false,
            },
            true,
            false,
        )
        .unwrap();
    core.write_at(handle, 0, b"unsaved draft").unwrap();
    let hashes = Rc::new(Cell::new(0));
    let seen = hashes.clone();
    let observer = core.clone();
    hash_hook::set(move || {
        // Synchronous, deterministic lock ownership check, not a speed gate.
        assert!(observer.namespace.try_write().is_ok());
        seen.set(seen.get() + 1);
    });
    let renamed = core.rename("draft", "document");
    hash_hook::clear();
    renamed.unwrap();
    assert_eq!(hashes.get(), 1);
    core.release(handle).unwrap();
    let reader = core.open("document", Access::Read, false, false).unwrap();
    assert_eq!(core.read_at(reader, 0, 100).unwrap(), b"unsaved draft");
    core.release(reader).unwrap();
}
