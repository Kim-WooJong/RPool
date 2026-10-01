use super::*;
use crate::storage::error::StorageErrorKind;
use crate::storage::reference::ObjectRef;
use crate::storage::registry::BackendRegistry;
use std::io::{self, Cursor};
use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

fn backend() -> Arc<LocalBackend> {
    Arc::new(LocalBackend::new(BackendId::new("local-test").unwrap()).unwrap())
}
fn key() -> ObjectKey {
    ObjectKey::new("nested/object").unwrap()
}
fn put(backend: &dyn StorageBackend, bytes: &[u8]) {
    backend
        .write(
            &OperationContext::none(),
            &key(),
            &mut Cursor::new(bytes),
            &WriteOptions::default(),
        )
        .unwrap();
}
fn get(backend: &dyn StorageBackend) -> Vec<u8> {
    backend
        .read_all(&OperationContext::none(), &key(), None)
        .unwrap()
}
fn assert_no_staging(backend: &LocalBackend) {
    for item in std::fs::read_dir(backend.directory.path().join("nested")).unwrap() {
        assert!(!item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX));
    }
}

#[test]
fn isolated_root_registry_and_basic_io() {
    let local = backend();
    let mut registry = BackendRegistry::new();
    registry.register(local.clone()).unwrap();
    let reference = ObjectRef::new(local.id(), key());
    let store = registry.resolve(&reference).unwrap().as_ref();
    put(store, b"");
    assert_eq!(
        store.stat(&OperationContext::none(), &key()).unwrap().size,
        0
    );
    put(store, b"abc");
    assert_eq!(get(store), b"abc");
    assert_no_staging(&local);
    store.delete(&OperationContext::none(), &key()).unwrap();
    assert_eq!(
        store
            .stat(&OperationContext::none(), &key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
    assert_eq!(
        store
            .delete(&OperationContext::none(), &key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
    assert_eq!(
        store
            .read(
                &OperationContext::none(),
                &key(),
                &ReadRange::new(0, 0).unwrap(),
                &mut Vec::new()
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
}

#[test]
fn eof_ranges_and_bounded_prefixes() {
    let local = backend();
    put(local.as_ref(), b"abcdef");
    for (offset, length, expected) in [
        (1, 3, &b"bcd"[..]),
        (4, 99, &b"ef"[..]),
        (6, 1, &b""[..]),
        (99, 0, &b""[..]),
        (u64::MAX, 0, &b""[..]),
        (2, 0, &b""[..]),
    ] {
        let mut sink = Vec::new();
        let receipt = local
            .read(
                &OperationContext::none(),
                &key(),
                &ReadRange::new(offset, length).unwrap(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(sink, expected);
        assert_eq!(receipt.bytes_read, expected.len() as u64);
    }
    for limit in [0, 3, 99] {
        assert_eq!(
            local
                .read_all(&OperationContext::none(), &key(), Some(limit))
                .unwrap(),
            &b"abcdef"[..limit.min(6)]
        );
    }
}

struct BrokenSource {
    emitted: bool,
}
impl Read for BrokenSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.emitted {
            return Err(io::Error::other("source failure"));
        }
        self.emitted = true;
        buffer.fill(42);
        Ok(buffer.len())
    }
}
#[test]
fn source_failure_preserves_old_or_absent_destination_and_cleans_stage() {
    for existing in [false, true] {
        let local = backend();
        if existing {
            put(local.as_ref(), b"old");
        }
        assert!(local
            .write(
                &OperationContext::none(),
                &key(),
                &mut BrokenSource { emitted: false },
                &WriteOptions::default()
            )
            .is_err());
        if existing {
            assert_eq!(get(local.as_ref()), b"old");
        } else {
            assert!(!local.directory.path().join("nested/object").exists());
        }
        assert_no_staging(&local);
    }
}

struct ObserveSource {
    backend: Arc<LocalBackend>,
    first: bool,
}
impl Read for ObserveSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert_eq!(get(self.backend.as_ref()), b"old");
        if !self.first {
            return Ok(0);
        }
        self.first = false;
        buffer[..3].copy_from_slice(b"new");
        Ok(3)
    }
}
#[test]
fn old_file_visible_until_complete_publication() {
    let local = backend();
    put(local.as_ref(), b"old");
    local
        .write(
            &OperationContext::none(),
            &key(),
            &mut ObserveSource {
                backend: local.clone(),
                first: true,
            },
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(get(local.as_ref()), b"new");
    assert_no_staging(&local);
}

struct CancelSource {
    flag: Arc<AtomicBool>,
}
impl Read for CancelSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.flag.store(true, Ordering::Release);
        buffer[0] = 1;
        Ok(1)
    }
}
struct BrokenSink;
impl Write for BrokenSink {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sink failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn cancellation_deadline_and_sink_error() {
    let local = backend();
    put(local.as_ref(), b"old");
    let flag = Arc::new(AtomicBool::new(false));
    assert_eq!(
        local
            .write(
                &OperationContext::with_cancel(flag.clone()),
                &key(),
                &mut CancelSource { flag },
                &WriteOptions::default()
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::Cancelled
    );
    assert_eq!(get(local.as_ref()), b"old");
    assert_no_staging(&local);
    assert_eq!(
        local
            .stat(&OperationContext::with_deadline(Instant::now()), &key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::Timeout
    );
    assert!(local
        .read(
            &OperationContext::none(),
            &key(),
            &ReadRange::new(0, 3).unwrap(),
            &mut BrokenSink
        )
        .is_err());
}

#[test]
fn aliases_and_directory_collisions_rejected() {
    let local = backend();
    for raw in [
        ".",
        "a/./b",
        "a//b",
        "a/",
        "C:foo",
        "a:b",
        ".rpool-stage-0",
        "a/\n",
    ] {
        let key = ObjectKey::new(raw).unwrap();
        assert_eq!(
            local
                .write(
                    &OperationContext::none(),
                    &key,
                    &mut Cursor::new(b"bad"),
                    &WriteOptions::default()
                )
                .unwrap_err()
                .kind(),
            StorageErrorKind::InvalidInput
        );
    }
    for raw in ["/absolute", "../escape", "a/../b", "\\root"] {
        assert!(ObjectKey::new(raw).is_err());
    }
    std::fs::create_dir_all(local.directory.path().join("nested/object")).unwrap();
    assert!(local
        .write(
            &OperationContext::none(),
            &key(),
            &mut Cursor::new(b"bad"),
            &WriteOptions::default()
        )
        .is_err());
    assert!(local.stat(&OperationContext::none(), &key()).is_err());
    std::fs::write(local.directory.path().join("file"), b"x").unwrap();
    assert!(local
        .write(
            &OperationContext::none(),
            &ObjectKey::new("file/child").unwrap(),
            &mut Cursor::new(b"bad"),
            &WriteOptions::default()
        )
        .is_err());
}

#[test]
fn symlink_leaf_parent_and_dangling_links_never_touch_external_data() {
    for variant in 0..3 {
        let local = backend();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("object"), b"outside").unwrap();
        if variant == 0 {
            symlink(outside.path(), local.directory.path().join("nested")).unwrap();
        } else {
            std::fs::create_dir(local.directory.path().join("nested")).unwrap();
            symlink(
                outside
                    .path()
                    .join(if variant == 1 { "object" } else { "absent" }),
                local.directory.path().join("nested/object"),
            )
            .unwrap();
        }
        let ctx = OperationContext::none();
        assert!(local.stat(&ctx, &key()).is_err());
        assert!(local.read_all(&ctx, &key(), None).is_err());
        assert!(local
            .write(
                &ctx,
                &key(),
                &mut Cursor::new(b"attack"),
                &WriteOptions::default()
            )
            .is_err());
        assert!(local.delete(&ctx, &key()).is_err());
        assert_eq!(
            std::fs::read(outside.path().join("object")).unwrap(),
            b"outside"
        );
        assert!(!outside.path().join("absent").exists());
    }
}

struct SwapParent {
    local: Arc<LocalBackend>,
    outside: std::path::PathBuf,
    swapped: bool,
}
impl Read for SwapParent {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.swapped {
            return Ok(0);
        }
        self.swapped = true;
        std::fs::rename(
            self.local.directory.path().join("nested"),
            self.local.directory.path().join("retained"),
        )?;
        symlink(&self.outside, self.local.directory.path().join("nested"))?;
        buffer[..3].copy_from_slice(b"new");
        Ok(3)
    }
}
#[test]
fn parent_symlink_swap_during_staging_keeps_descriptor_destination() {
    let local = backend();
    put(local.as_ref(), b"old");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("object"), b"outside").unwrap();
    local
        .write(
            &OperationContext::none(),
            &key(),
            &mut SwapParent {
                local: local.clone(),
                outside: outside.path().to_path_buf(),
                swapped: false,
            },
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(outside.path().join("object")).unwrap(),
        b"outside"
    );
    assert_eq!(
        std::fs::read(local.directory.path().join("retained/object")).unwrap(),
        b"new"
    );
    assert_eq!(
        std::fs::read_dir(local.directory.path().join("retained"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn conditional_create_is_atomic_and_optional_operations_are_honest() {
    let local = backend();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for bytes in [b"one", b"two"] {
        let local = local.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            local.write(
                &OperationContext::none(),
                &key(),
                &mut Cursor::new(bytes),
                &WriteOptions {
                    overwrite: false,
                    expected_version: None,
                },
            )
        }));
    }
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .next()
            .unwrap()
            .kind(),
        StorageErrorKind::AlreadyExists
    );
    assert_no_staging(&local);
    let ctx = OperationContext::none();
    assert_eq!(
        local
            .write(
                &ctx,
                &key(),
                &mut Cursor::new(b"bad"),
                &WriteOptions {
                    overwrite: true,
                    expected_version: Some("x".into())
                }
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::Unsupported
    );
    assert_eq!(
        local.list(&ctx, "", None).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    assert_eq!(
        local.copy(&ctx, &key(), &key()).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    assert_eq!(
        local.rename(&ctx, &key(), &key()).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    let caps = local.capabilities();
    assert!(caps.atomic_replace.is_supported());
    assert!(caps.conditional_create.is_supported());
    for cap in [
        caps.conditional_update,
        caps.conditional_delete,
        caps.version_pinning,
        caps.durable_after_write,
    ] {
        assert!(!cap.is_supported())
    }
    assert_eq!(caps.consistency_scope, ConsistencyScope::ProcessLocal);
}

#[test]
fn special_socket_is_rejected() {
    let local = backend();
    std::fs::create_dir(local.directory.path().join("nested")).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(local.directory.path().join("nested/object"))
            .unwrap();
    assert!(local
        .read_all(&OperationContext::none(), &key(), None)
        .is_err());
}
