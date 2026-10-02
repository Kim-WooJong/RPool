use super::faults::{Fault, FaultBackend, Gate, Operation, Rule};
use super::*;
use crate::storage::error::StorageErrorKind;
use crate::storage::reference::ObjectRef;
use crate::storage::registry::BackendRegistry;
use std::io::{self, Cursor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Barrier;
use std::thread;
use std::time::Instant;

fn backend() -> Arc<MemoryBackend> {
    Arc::new(MemoryBackend::new(BackendId::new("memory-test").unwrap()))
}
fn key() -> ObjectKey {
    ObjectKey::new("objects/a").unwrap()
}
fn put(backend: &dyn StorageBackend, bytes: &[u8]) -> WriteReceipt {
    backend
        .write(
            &OperationContext::none(),
            &key(),
            &mut Cursor::new(bytes),
            &WriteOptions::default(),
        )
        .unwrap()
}
fn get(backend: &dyn StorageBackend) -> Vec<u8> {
    backend
        .read_all(&OperationContext::none(), &key(), None)
        .unwrap()
}
fn wrapper(
    inner: Arc<dyn StorageBackend>,
    operation: Operation,
    call: u64,
    fault: Fault,
) -> FaultBackend {
    FaultBackend::new(
        inner,
        vec![Rule {
            operation,
            call,
            fault,
        }],
    )
    .unwrap()
}

#[test]
fn registry_injects_real_trait_and_rejects_duplicate_identity() {
    let memory = backend();
    let mut registry = BackendRegistry::new();
    registry.register(memory.clone()).unwrap();
    assert_eq!(
        registry.register(memory.clone()).unwrap_err().kind(),
        StorageErrorKind::AlreadyExists
    );
    let reference = ObjectRef::new(memory.id(), key());
    let injected = registry.resolve(&reference).unwrap();
    put(injected.as_ref(), b"payload");
    assert_eq!(get(injected.as_ref()), b"payload");
}

#[test]
fn empty_overwrite_delete_and_versions() {
    let memory = backend();
    let ctx = OperationContext::none();
    let first = put(memory.as_ref(), b"");
    assert_eq!(memory.stat(&ctx, &key()).unwrap().size, 0);
    assert_eq!(get(memory.as_ref()), b"");
    let second = put(memory.as_ref(), b"abc");
    assert_ne!(first.version, second.version);
    memory.delete(&ctx, &key()).unwrap();
    assert_eq!(
        memory.delete(&ctx, &key()).unwrap_err().kind(),
        StorageErrorKind::NotFound
    );
    assert_eq!(
        memory.stat(&ctx, &key()).unwrap_err().kind(),
        StorageErrorKind::NotFound
    );
    let third = put(memory.as_ref(), b"new");
    assert_ne!(third.version, second.version);
    assert_eq!(get(memory.as_ref()), b"new");
}

#[test]
fn bounds_eof_and_prefix_semantics() {
    let memory = backend();
    put(memory.as_ref(), b"abcdef");
    let ctx = OperationContext::none();
    for (offset, length, expected) in [
        (1, 3, &b"bcd"[..]),
        (4, 99, &b"ef"[..]),
        (6, 1, &b""[..]),
        (99, 0, &b""[..]),
        (u64::MAX, 0, &b""[..]),
        (2, 0, &b""[..]),
    ] {
        let mut sink = Vec::new();
        let receipt = memory
            .read(
                &ctx,
                &key(),
                &ReadRange::new(offset, length).unwrap(),
                &mut sink,
            )
            .unwrap();
        assert_eq!(sink, expected);
        assert_eq!(receipt.bytes_read, expected.len() as u64);
    }
    assert_eq!(memory.read_all(&ctx, &key(), Some(3)).unwrap(), b"abc");
    assert!(memory.read_all(&ctx, &key(), Some(0)).unwrap().is_empty());
    assert_eq!(memory.read_all(&ctx, &key(), Some(99)).unwrap(), b"abcdef");
    assert!(ReadRange::new(u64::MAX, 1).is_err());
    memory.delete(&ctx, &key()).unwrap();
    assert_eq!(
        memory
            .read(
                &ctx,
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
fn conditional_create_update_and_exhaustion_preserve_data() {
    let memory = backend();
    let ctx = OperationContext::none();
    let create = WriteOptions {
        overwrite: false,
        expected_version: None,
        defer_hash_check: false,
    };
    let first = memory
        .write(&ctx, &key(), &mut Cursor::new(b"one"), &create)
        .unwrap();
    assert_eq!(
        memory
            .write(&ctx, &key(), &mut Cursor::new(b"two"), &create)
            .unwrap_err()
            .kind(),
        StorageErrorKind::AlreadyExists
    );
    let update = WriteOptions {
        overwrite: true,
        expected_version: first.version,
        defer_hash_check: false,
    };
    memory
        .write(&ctx, &key(), &mut Cursor::new(b"two"), &update)
        .unwrap();
    assert_eq!(
        memory
            .write(&ctx, &key(), &mut Cursor::new(b"stale"), &update)
            .unwrap_err()
            .kind(),
        StorageErrorKind::PreconditionFailed
    );
    let invalid = WriteOptions {
        overwrite: false,
        expected_version: Some("1".into()),
        defer_hash_check: false,
    };
    assert_eq!(
        memory
            .write(&ctx, &key(), &mut Cursor::new(b"bad"), &invalid)
            .unwrap_err()
            .kind(),
        StorageErrorKind::InvalidInput
    );
    memory.state.lock().unwrap().generation = u64::MAX;
    assert_eq!(
        memory
            .write(
                &ctx,
                &key(),
                &mut Cursor::new(b"overflow"),
                &WriteOptions::default()
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::Other
    );
    assert_eq!(get(memory.as_ref()), b"two");
}

struct RendezvousSource {
    barrier: Arc<Barrier>,
    bytes: Cursor<Vec<u8>>,
    waited: bool,
}
impl Read for RendezvousSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.waited {
            self.waited = true;
            self.barrier.wait();
        }
        self.bytes.read(buffer)
    }
}

#[test]
fn competing_cas_is_checked_at_commit() {
    let memory = backend();
    let first = put(memory.as_ref(), b"initial");
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for value in [b"a", b"b"] {
        let memory = memory.clone();
        let barrier = barrier.clone();
        let expected_version = first.version.clone();
        workers.push(thread::spawn(move || {
            let mut source = RendezvousSource {
                barrier,
                bytes: Cursor::new(value.to_vec()),
                waited: false,
            };
            memory.write(
                &OperationContext::none(),
                &key(),
                &mut source,
                &WriteOptions {
                    overwrite: true,
                    expected_version,
                    defer_hash_check: false,
                },
            )
        }));
    }
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .next()
            .unwrap()
            .kind(),
        StorageErrorKind::PreconditionFailed
    );
}

struct ReplacingSink {
    backend: Arc<MemoryBackend>,
    bytes: Vec<u8>,
    replaced: bool,
}
impl Write for ReplacingSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.replaced {
            self.replaced = true;
            put(self.backend.as_ref(), b"replacement");
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn read_snapshot_survives_reentrant_overwrite() {
    let memory = backend();
    let original = vec![42; CHUNK * 2 + 1];
    let version = put(memory.as_ref(), &original).version;
    let mut sink = ReplacingSink {
        backend: memory.clone(),
        bytes: Vec::new(),
        replaced: false,
    };
    let receipt = memory
        .read(
            &OperationContext::none(),
            &key(),
            &ReadRange::new(0, original.len() as u64).unwrap(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.bytes, original);
    assert_eq!(receipt.version, version);
    assert_eq!(get(memory.as_ref()), b"replacement");
}

#[test]
fn partial_source_failure_preserves_old_object() {
    let memory = backend();
    let original = put(memory.as_ref(), b"old");
    let faulty = wrapper(memory.clone(), Operation::Write, 1, Fault::PartialWrite(2));
    let error = faulty
        .write(
            &OperationContext::none(),
            &key(),
            &mut Cursor::new(b"new data"),
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::TransientIo);
    assert_eq!(get(memory.as_ref()), b"old");
    assert_eq!(
        memory
            .stat(&OperationContext::none(), &key())
            .unwrap()
            .version,
        original.version
    );
    put(&faulty, b"next call succeeds");
}

#[test]
fn faults_select_operation_and_one_based_call_once() {
    let memory = backend();
    put(memory.as_ref(), b"abc");
    for error in [
        StorageError::Timeout {
            detail: "injected".into(),
        },
        StorageError::PermissionDenied {
            path: "test".into(),
        },
        StorageError::not_found("test"),
        StorageError::Cancelled {
            detail: "injected".into(),
        },
        StorageError::CorruptData {
            found: "bad".into(),
            expected: "good".into(),
        },
    ] {
        let kind = error.kind();
        let faulty = wrapper(memory.clone(), Operation::Stat, 2, Fault::Error(error));
        let ctx = OperationContext::none();
        assert_eq!(get(&faulty), b"abc");
        faulty.stat(&ctx, &key()).unwrap();
        assert_eq!(faulty.stat(&ctx, &key()).unwrap_err().kind(), kind);
        faulty.stat(&ctx, &key()).unwrap();
    }
}

#[test]
fn corrupt_and_short_reads_are_observable() {
    let memory = backend();
    put(memory.as_ref(), b"abcdef");
    let faulty = wrapper(memory.clone(), Operation::Read, 1, Fault::CorruptRead);
    let mut sink = Vec::new();
    let receipt = faulty
        .read(
            &OperationContext::none(),
            &key(),
            &ReadRange::new(0, 6).unwrap(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(receipt.bytes_read, 6);
    assert_ne!(blake3::hash(&sink), blake3::hash(b"abcdef"));
    assert_eq!(get(memory.as_ref()), b"abcdef");
    let faulty = wrapper(memory.clone(), Operation::Read, 1, Fault::ShortRead(2));
    sink.clear();
    let receipt = faulty
        .read(
            &OperationContext::none(),
            &key(),
            &ReadRange::new(0, 6).unwrap(),
            &mut sink,
        )
        .unwrap();
    assert_eq!(receipt.bytes_read, 2);
    assert_eq!(sink, b"ab");
    let faulty = wrapper(memory, Operation::ReadAll, 1, Fault::ShortRead(1));
    assert_eq!(get(&faulty), b"a");
}

#[test]
fn lost_write_and_delete_responses_do_not_claim_rollback() {
    let memory = backend();
    let faulty = wrapper(
        memory.clone(),
        Operation::Write,
        1,
        Fault::LoseResponse(None),
    );
    let error = faulty
        .write(
            &OperationContext::none(),
            &key(),
            &mut Cursor::new(b"committed"),
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnknownOutcome);
    assert!(!error.is_retriable());
    assert_eq!(get(memory.as_ref()), b"committed");
    let faulty = wrapper(
        memory.clone(),
        Operation::Delete,
        1,
        Fault::LoseResponse(None),
    );
    assert_eq!(
        faulty
            .delete(&OperationContext::none(), &key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::UnknownOutcome
    );
    assert_eq!(
        memory
            .stat(&OperationContext::none(), &key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
}

#[test]
fn cancellation_before_and_after_commit_is_distinguished_without_sleep() {
    for after_commit in [false, true] {
        let memory = backend();
        put(memory.as_ref(), b"old");
        let gate = Gate::new();
        let fault = if after_commit {
            Fault::LoseResponse(Some(gate.clone()))
        } else {
            Fault::PauseBefore(gate.clone())
        };
        let faulty = wrapper(memory.clone(), Operation::Write, 1, fault);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let worker = thread::spawn(move || {
            faulty.write(
                &OperationContext::with_cancel(flag),
                &key(),
                &mut Cursor::new(b"new"),
                &WriteOptions::default(),
            )
        });
        gate.reached.wait();
        cancelled.store(true, Ordering::Release);
        gate.release.wait();
        let error = worker.join().unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            if after_commit {
                StorageErrorKind::UnknownOutcome
            } else {
                StorageErrorKind::Cancelled
            }
        );
        assert_eq!(
            get(memory.as_ref()),
            if after_commit { b"new" } else { b"old" }
        );
    }
}

struct CancellingSource {
    flag: Arc<AtomicBool>,
}
impl Read for CancellingSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.flag.store(true, Ordering::Release);
        buffer[0] = 1;
        Ok(1)
    }
}
struct BrokenSink;
impl Write for BrokenSink {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("test sink failed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn midstream_cancel_deadline_and_sink_errors() {
    let memory = backend();
    put(memory.as_ref(), b"old");
    let flag = Arc::new(AtomicBool::new(false));
    let error = memory
        .write(
            &OperationContext::with_cancel(flag.clone()),
            &key(),
            &mut CancellingSource { flag },
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::Cancelled);
    assert_eq!(get(memory.as_ref()), b"old");
    let expired = OperationContext::with_deadline(Instant::now());
    assert_eq!(
        memory.stat(&expired, &key()).unwrap_err().kind(),
        StorageErrorKind::Timeout
    );
    assert_eq!(
        memory
            .read(
                &OperationContext::none(),
                &key(),
                &ReadRange::new(0, 3).unwrap(),
                &mut BrokenSink
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::TransientIo
    );
}

#[test]
fn capabilities_and_unsupported_operations_are_honest() {
    let memory = backend();
    let caps = memory.capabilities();
    assert_eq!(caps.consistency_scope, ConsistencyScope::ProcessLocal);
    assert!(caps.conditional_create.is_supported());
    assert!(caps.conditional_update.is_supported());
    for capability in [
        caps.version_pinning,
        caps.conditional_delete,
        caps.durable_after_write,
        caps.list,
        caps.copy_same_backend,
        caps.rename,
    ] {
        assert_eq!(capability, Capability::Unsupported);
    }
    let ctx = OperationContext::none();
    assert_eq!(
        memory.list(&ctx, "", None).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    assert_eq!(
        memory.copy(&ctx, &key(), &key()).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    assert_eq!(
        memory.rename(&ctx, &key(), &key()).unwrap_err().kind(),
        StorageErrorKind::Unsupported
    );
    assert!(FaultBackend::new(
        memory.clone(),
        vec![Rule {
            operation: Operation::Stat,
            call: 0,
            fault: Fault::ShortRead(1)
        }]
    )
    .is_err());
    assert!(FaultBackend::new(
        memory,
        vec![Rule {
            operation: Operation::Stat,
            call: 1,
            fault: Fault::PartialWrite(1)
        }]
    )
    .is_err());
}
