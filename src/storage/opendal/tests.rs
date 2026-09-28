use super::*;
use crate::storage::error::StorageErrorKind as K;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
fn backend() -> OpenDalMemory {
    OpenDalMemory::new(BackendId::new("prototype").unwrap()).unwrap()
}
fn key(s: &str) -> ObjectKey {
    ObjectKey::new(s).unwrap()
}
fn put(b: &OpenDalMemory, data: &[u8]) {
    b.write(
        &OperationContext::none(),
        &key("a/b"),
        &mut &data[..],
        &WriteOptions::default(),
    )
    .unwrap();
}
#[test]
fn round_trip_isolation_overwrite_and_delete() {
    let b = backend();
    let other = backend();
    let ctx = OperationContext::none();
    let bytes: Vec<u8> = (0..CHUNK * 3 + 17).map(|n| n as u8).collect();
    put(&b, &bytes);
    assert_eq!(b.read_all(&ctx, &key("a/b"), None).unwrap(), bytes);
    assert_eq!(
        other.stat(&ctx, &key("a/b")).unwrap_err().kind(),
        K::NotFound
    );
    put(&b, b"");
    assert_eq!(b.stat(&ctx, &key("a/b")).unwrap().size, 0);
    b.delete(&ctx, &key("a/b")).unwrap();
    assert_eq!(b.delete(&ctx, &key("a/b")).unwrap_err().kind(), K::NotFound);
}
#[test]
fn ranges_and_limits_follow_eof_contract() {
    let b = backend();
    put(&b, b"abcdef");
    let ctx = OperationContext::none();
    for (offset, len, expected) in [
        (2, 99, &b"cdef"[..]),
        (6, 1, &b""[..]),
        (99, 1, &b""[..]),
        (1, 0, &b""[..]),
    ] {
        let mut out = Vec::new();
        let r = b
            .read(
                &ctx,
                &key("a/b"),
                &ReadRange::new(offset, len).unwrap(),
                &mut out,
            )
            .unwrap();
        assert_eq!(out, expected);
        assert_eq!(r.bytes_read, expected.len() as u64);
    }
    assert_eq!(b.read_all(&ctx, &key("a/b"), Some(3)).unwrap(), b"abc");
    assert!(b.read_all(&ctx, &key("a/b"), Some(0)).unwrap().is_empty());
    assert_eq!(
        b.read(
            &ctx,
            &key("missing"),
            &ReadRange::new(0, 0).unwrap(),
            &mut Vec::new()
        )
        .unwrap_err()
        .kind(),
        K::NotFound
    );
}
#[test]
fn normalization_aliases_are_rejected() {
    let b = backend();
    put(&b, b"original");
    let ctx = OperationContext::none();
    for s in ["a//b", "a/./b", "a/b/", " a/b", "a/b ", "a:b", "a/\t/b"] {
        assert_eq!(
            b.stat(&ctx, &key(s)).unwrap_err().kind(),
            K::InvalidInput,
            "{}",
            s
        );
    }
    assert_eq!(b.read_all(&ctx, &key("a/b"), None).unwrap(), b"original");
}
struct NeverRead;
impl Read for NeverRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("unsupported operation consumed source")
    }
}
#[test]
fn capabilities_and_unsupported_operations() {
    let b = backend();
    let ctx = OperationContext::none();
    let k = key("x");
    for opts in [
        WriteOptions {
            overwrite: false,
            expected_version: None,
        },
        WriteOptions {
            overwrite: true,
            expected_version: Some("v".into()),
        },
    ] {
        assert_eq!(
            b.write(&ctx, &k, &mut NeverRead, &opts).unwrap_err().kind(),
            K::Unsupported
        );
    }
    assert_eq!(b.list(&ctx, "", None).unwrap_err().kind(), K::Unsupported);
    assert_eq!(b.copy(&ctx, &k, &k).unwrap_err().kind(), K::Unsupported);
    assert_eq!(b.rename(&ctx, &k, &k).unwrap_err().kind(), K::Unsupported);
    let c = b.capabilities();
    assert!(!c.conditional_create.is_supported());
    assert!(!c.durable_after_write.is_supported());
    assert_eq!(c.max_object_size, Some(OBJECT_LIMIT));
}
struct Fails(bool);
impl Read for Fails {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.0 {
            return Err(std::io::Error::other("source detail"));
        }
        self.0 = true;
        out[0] = 7;
        Ok(1)
    }
}
#[test]
fn failed_and_oversized_writes_preserve_previous_object() {
    let b = backend();
    put(&b, b"old");
    let ctx = OperationContext::none();
    assert!(b
        .write(
            &ctx,
            &key("a/b"),
            &mut Fails(false),
            &WriteOptions::default()
        )
        .is_err());
    assert_eq!(b.read_all(&ctx, &key("a/b"), None).unwrap(), b"old");
    let data = vec![3; OBJECT_LIMIT as usize];
    put(&b, &data);
    assert_eq!(
        b.write(
            &ctx,
            &key("a/b"),
            &mut std::io::repeat(4).take(OBJECT_LIMIT + 1),
            &WriteOptions::default()
        )
        .unwrap_err()
        .kind(),
        K::InvalidInput
    );
    assert_eq!(b.read_all(&ctx, &key("a/b"), None).unwrap(), data);
}
struct Cancels(Arc<AtomicBool>);
impl Read for Cancels {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        out[0] = 8;
        self.0.store(true, Ordering::Release);
        Ok(1)
    }
}
#[test]
fn cancellation_and_deadline_do_not_publish() {
    let b = backend();
    put(&b, b"old");
    let flag = Arc::new(AtomicBool::new(false));
    let ctx = OperationContext::with_cancel(flag.clone());
    assert_eq!(
        b.write(
            &ctx,
            &key("a/b"),
            &mut Cancels(flag),
            &WriteOptions::default()
        )
        .unwrap_err()
        .kind(),
        K::Cancelled
    );
    assert_eq!(
        b.read_all(&OperationContext::none(), &key("a/b"), None)
            .unwrap(),
        b"old"
    );
    assert_eq!(
        b.stat(
            &OperationContext::with_deadline(std::time::Instant::now()),
            &key("a/b")
        )
        .unwrap_err()
        .kind(),
        K::Timeout
    );
}
#[test]
fn admission_wait_observes_deadline_and_reentrancy() {
    let b = Arc::new(backend());
    let ctx = OperationContext::none();
    let permit = b.admit(&ctx).unwrap();
    assert_eq!(b.stat(&ctx, &key("x")).unwrap_err().kind(), K::InvalidInput);
    let other = b.clone();
    let t = std::thread::spawn(move || {
        other
            .stat(
                &OperationContext::with_deadline(
                    std::time::Instant::now() + Duration::from_millis(30),
                ),
                &key("x"),
            )
            .unwrap_err()
            .kind()
    });
    assert_eq!(t.join().unwrap(), K::Timeout);
    drop(permit);
    put(&b, b"usable");
}
#[test]
fn nested_runtime_is_rejected_and_drop_is_safe() {
    let b = backend();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async move {
        assert_eq!(
            b.stat(&OperationContext::none(), &key("x"))
                .unwrap_err()
                .kind(),
            K::Unsupported
        );
        assert!(OpenDalMemory::new(BackendId::new("x").unwrap()).is_err());
        drop(b);
    });
}
#[test]
fn sdk_error_mapping_is_sanitized() {
    let e = map_error(::opendal::Error::new(
        ::opendal::ErrorKind::PermissionDenied,
        "private-marker",
    ));
    assert_eq!(e.kind(), K::PermissionDenied);
    assert!(!e.to_string().contains("private-marker"));
}

#[test]
fn real_adapter_runs_verified_storage_service() {
    use crate::models::{Shard, ShardKind};
    use crate::storage::{
        reader::StorageReader, reference::ObjectRef, registry::BackendRegistry,
        writer::StorageWriter,
    };
    let b = Arc::new(backend());
    let mut registry = BackendRegistry::new();
    registry.register(b.clone()).unwrap();
    let raw = "synthetic:Exact/Address";
    let bindings =
        std::collections::BTreeMap::from([(raw.to_owned(), ObjectRef::new(b.id(), key("a/b")))]);
    let writer = StorageWriter::synthetic(StorageReader::from_registry(
        registry,
        bindings,
        OperationContext::none(),
    ));
    let data = vec![42; CHUNK + 31];
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source");
    std::fs::write(&path, &data).unwrap();
    let shard = Shard {
        index: 0,
        offset: 0,
        size: data.len() as u64,
        remote: "synthetic:".into(),
        object: raw.into(),
        blake3: blake3::hash(&data).to_hex().to_string(),
        kind: ShardKind::Data,
        group: 0,
        slot: 0,
    };
    writer.write_file(&path, 0, &shard, 1).unwrap();
    writer.reader().verify(&shard, true).unwrap();
    put(&b, &vec![43; data.len()]);
    assert!(writer.reader().verify(&shard, true).is_err());
    writer.write_file(&path, 0, &shard, 1).unwrap();
    writer.reader().verify(&shard, true).unwrap();
}

#[test]
fn cancellation_at_eof_aborts_staged_bytes_and_sink_failure_releases_gate() {
    struct AtEof {
        flag: Arc<AtomicBool>,
        sent: bool,
    }
    impl Read for AtEof {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.sent {
                self.flag.store(true, Ordering::Release);
                Ok(0)
            } else {
                self.sent = true;
                out[0] = 9;
                Ok(1)
            }
        }
    }
    struct BadSink;
    impl Write for BadSink {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("failed sink"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let b = backend();
    put(&b, b"old");
    let flag = Arc::new(AtomicBool::new(false));
    assert_eq!(
        b.write(
            &OperationContext::with_cancel(flag.clone()),
            &key("a/b"),
            &mut AtEof { flag, sent: false },
            &WriteOptions::default()
        )
        .unwrap_err()
        .kind(),
        K::Cancelled
    );
    let ctx = OperationContext::none();
    assert!(b
        .read(
            &ctx,
            &key("a/b"),
            &ReadRange::new(0, 4).unwrap(),
            &mut BadSink
        )
        .is_err());
    assert_eq!(b.read_all(&ctx, &key("a/b"), None).unwrap(), b"old");
}
