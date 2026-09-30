//! Synthetic rclone caches in the layout rclone 1.75 writes (checked in
//! Docker): `vfs/:webdav{id}/<path>` data and `vfsMeta/:webdav{id}/<path>`.
use super::*;
use crate::mount::virtual_drive::{fixture, fixture_reopen};

const FS: &str = ":webdav{vK-qC}";

fn cache_file(root: &Path, rel: &str, data: Option<&[u8]>, meta: Option<&str>) {
    let cache = root.join("vfs-cache");
    if let Some(data) = data {
        let path = cache.join("vfs").join(FS).join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    if let Some(meta) = meta {
        let path = cache.join("vfsMeta").join(FS).join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, meta).unwrap();
    }
}
fn meta(size: u64, cached: u64, dirty: bool) -> String {
    format!(
        r#"{{"ModTime":"2026-09-30T05:06:38Z","ATime":"2026-09-30T05:06:38Z","Size":{size},"Rs":[{{"Pos":0,"Size":{cached}}}],"Fingerprint":"","Dirty":{dirty}}}"#
    )
}
fn dirty(root: &Path, rel: &str, bytes: &[u8]) {
    let n = bytes.len() as u64;
    cache_file(root, rel, Some(bytes), Some(&meta(n, n, true)));
}
fn put(drive: &VirtualDrive, path: &str, bytes: &[u8]) {
    let intent = drive.begin(path).unwrap();
    fs::write(drive.spool_path(&intent), bytes).unwrap();
    drive.seal(intent).unwrap();
}
fn contents(drive: &VirtualDrive, path: &str) -> Vec<u8> {
    let revision = drive.view().unwrap()[path].clone();
    drive.read(&revision, 0, 1 << 20).unwrap()
}
fn frozen(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("recovered-native-cache");
    if !dir.exists() {
        return Vec::new();
    }
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[test]
fn clean_cache_is_removed_without_a_report() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    cache_file(
        root.path(),
        "read.txt",
        Some(b"cached"),
        Some(&meta(6, 6, false)),
    );
    cache_file(root.path(), "dir/empty-orphan", Some(b""), None);
    assert!(drive.recover_previous_cache().unwrap().is_empty());
    assert!(!root.path().join("vfs-cache").exists());
    assert!(frozen(root.path()).is_empty(), "nothing left to keep");
    assert!(drive.state.lock().unwrap().pending.is_empty());
}

#[test]
fn local_drive_imports_dirty_writes_in_place() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    put(&drive, "edit.txt", b"v1\n");
    dirty(root.path(), "edit.txt", b"edited v2\n");
    dirty(root.path(), "dir/new.txt", b"brand new\n");
    cache_file(root.path(), "empty.txt", None, Some(&meta(0, 0, true)));
    let reports = drive.recover_previous_cache().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].imported,
        ["dir/new.txt", "edit.txt", "empty.txt"]
    );
    assert!(reports[0].removed && reports[0].kept.is_empty());
    assert_eq!(contents(&drive, "edit.txt"), b"edited v2\n");
    assert_eq!(contents(&drive, "dir/new.txt"), b"brand new\n");
    assert_eq!(contents(&drive, "empty.txt"), b"");
    let saved: Vec<RecoveryReport> =
        serde_json::from_slice(&fs::read(root.path().join(".rpool").join(REPORT)).unwrap())
            .unwrap();
    assert_eq!(saved, reports);
    assert!(
        drive.recover_previous_cache().unwrap().is_empty(),
        "idempotent"
    );
}

#[test]
fn sparse_unknown_and_invalid_entries_are_kept_not_imported() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    cache_file(
        root.path(),
        "sparse.bin",
        Some(&[1; 10]),
        Some(&meta(100, 10, true)),
    );
    cache_file(root.path(), "orphan.bin", Some(b"unsaved?"), None);
    cache_file(root.path(), "bad.json", Some(b"x"), Some("{not json"));
    dirty(root.path(), "CON.txt", b"reserved name");
    let reports = drive.recover_previous_cache().unwrap();
    let kept: Vec<&str> = reports[0].kept.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(kept, ["CON.txt", "bad.json", "orphan.bin", "sparse.bin"]);
    assert!(!reports[0].removed);
    assert!(drive.state.lock().unwrap().pending.is_empty());
    let dir = &reports[0].dir;
    assert!(
        dir.join("vfs").join(FS).join("orphan.bin").exists(),
        "bytes are kept"
    );
    // A second run neither repeats nor reports it.
    assert!(drive.recover_previous_cache().unwrap().is_empty());
    assert!(dir.exists());
}

/// Commits `path`'s pending write, as `sync` does after uploading.
fn commit(drive: &VirtualDrive, path: &str) {
    let intent = drive
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .find(|i| i.path == path)
        .cloned()
        .unwrap();
    let bytes = fs::read(drive.spool_path(&intent)).unwrap();
    drive
        .commit_uploaded(&intent, Some(crate::mount::virtual_tests::content(&bytes)))
        .unwrap();
}

#[test]
fn pool_sync_without_a_base_becomes_a_recovered_copy() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    drive.state.lock().unwrap().version = 6;
    put(&drive, "doc.txt", b"synced version\n");
    commit(&drive, "doc.txt");
    put(&drive, "mine.txt", b"own unsynced edit\n");
    // Neither path was opened for reading in the WebDAV session.
    drive.state.lock().unwrap().bases.clear();
    dirty(root.path(), "doc.txt", b"my unsaved edit\n");
    dirty(root.path(), "mine.txt", b"second unsaved edit\n");
    let before = revision_hash(&drive.view().unwrap()["doc.txt"]).unwrap();
    let reports = drive.recover_previous_cache().unwrap();
    assert_eq!(
        reports[0].imported,
        ["mine.txt"],
        "own unsynced head is safe"
    );
    let (from, to) = &reports[0].copied[0];
    assert_eq!(from, "doc.txt");
    assert!(
        to.starts_with("doc (recovered ") && to.ends_with(").txt"),
        "{to}"
    );
    let after = revision_hash(&drive.view().unwrap()["doc.txt"]).unwrap();
    assert_eq!(
        before, after,
        "a cloud head is never overwritten without a base"
    );
    assert_eq!(contents(&drive, to), b"my unsaved edit\n");
    assert_eq!(contents(&drive, "mine.txt"), b"second unsaved edit\n");
}

#[test]
fn pool_sync_with_a_read_base_imports_in_place() {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    drive.state.lock().unwrap().version = 6;
    put(&drive, "read.txt", b"base\n");
    commit(&drive, "read.txt");
    let head = drive.view().unwrap()["read.txt"].clone();
    drive.state.lock().unwrap().bases.clear();
    drive.pin_read("read.txt", &head).unwrap();
    dirty(root.path(), "read.txt", b"edited after read\n");
    let reports = drive.recover_previous_cache().unwrap();
    assert_eq!(reports[0].imported, ["read.txt"]);
    assert_eq!(contents(&drive, "read.txt"), b"edited after read\n");
    let intent = drive.state.lock().unwrap().pending.last().cloned().unwrap();
    assert_eq!(
        intent.parents,
        [head.id().to_string()],
        "descends from the read"
    );
}

#[test]
fn legacy_folders_only_produce_copies_and_equal_content_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    put(&drive, "a.txt", b"current\n");
    put(&drive, "same.txt", b"same\n");
    let legacy = root.path().join("recovered-native-cache").join("oldcache");
    for (rel, bytes) in [("a.txt", &b"old unsaved\n"[..]), ("same.txt", b"same\n")] {
        let n = bytes.len() as u64;
        let data = legacy.join("vfs").join(FS).join(rel);
        fs::create_dir_all(data.parent().unwrap()).unwrap();
        fs::write(&data, bytes).unwrap();
        let m = legacy.join("vfsMeta").join(FS).join(rel);
        fs::create_dir_all(m.parent().unwrap()).unwrap();
        fs::write(m, meta(n, n, true)).unwrap();
    }
    let reports = drive.recover_previous_cache().unwrap();
    assert!(reports[0].imported.is_empty());
    assert_eq!(reports[0].copied.len(), 1);
    assert_eq!(reports[0].skipped, 1);
    assert_eq!(contents(&drive, "a.txt"), b"current\n");
    assert_eq!(contents(&drive, &reports[0].copied[0].1), b"old unsaved\n");
}

#[test]
fn rerun_after_a_crash_before_the_journal_makes_no_duplicate() {
    let root = tempfile::tempdir().unwrap();
    {
        let mut drive = fixture(root.path());
        drive.pool_sync_roots = vec!["crypt:pool".into()];
        put(&drive, "doc.txt", b"head\n");
        drive.state.lock().unwrap().bases.clear();
        drive.state.lock().unwrap().save(root.path()).unwrap();
        dirty(root.path(), "doc.txt", b"unsaved\n");
        crate::mount::crash::arm("cache_recovery.before_journal");
        assert!(drive.recover_previous_cache().is_err());
        crate::mount::crash::disarm();
    }
    let mut drive = fixture_reopen(root.path());
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    let reports = drive.recover_previous_cache().unwrap();
    assert_eq!(reports[0].skipped, 1, "the sealed copy is found by hash");
    let pending = drive.state.lock().unwrap().pending.len();
    assert_eq!(pending, 2, "head plus one recovered copy");
}

#[test]
fn recovered_names_keep_folder_and_extension() {
    assert_eq!(
        recovered_name("a/b.tar.gz", "1234abcd", "ffeedd99"),
        "a/b.tar (recovered 1234abcd-ffeedd).gz"
    );
    assert_eq!(
        recovered_name("README", "t", "abc"),
        "README (recovered t-abc)"
    );
    assert_eq!(
        recovered_name(".env", "t", "abcdef"),
        ".env (recovered t-abcdef)"
    );
}
