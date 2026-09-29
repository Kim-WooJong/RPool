use super::*;
use crate::storage::error::StorageErrorKind;
use crate::storage::reference::ObjectRef;
use crate::storage::registry::BackendRegistry;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

fn executable() -> PathBuf {
    static FIXTURE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let dir = tempfile::Builder::new()
                .prefix("rpool fake process ")
                .tempdir()
                .unwrap();
            let source = dir.path().join("fixture.rs");
            std::fs::write(&source, include_str!("fixture.rs.txt")).unwrap();
            let exe = dir.path().join(if cfg!(windows) {
                "fake rclone.exe"
            } else {
                "fake rclone"
            });
            let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg("--edition=2021")
                .arg(&source)
                .arg("-o")
                .arg(&exe)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "fixture compilation: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            (dir, exe)
        })
        .1
        .clone()
}
fn setup(config: &str) -> (tempfile::TempDir, RcloneContext) {
    let dir = tempfile::Builder::new()
        .prefix("rpool config spaces ")
        .tempdir()
        .unwrap();
    let path = dir.path().join("config file.json");
    std::fs::write(&path, config).unwrap();
    let mut context = RcloneContext::new(executable(), ConfigSelection::File(path));
    context.environment.retain(|(k, _)| {
        !k.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
    });
    (dir, context)
}
fn normal() -> (tempfile::TempDir, RcloneContext) {
    setup(r#"{"crypt":{"type":"crypt"}}"#)
}
fn deadline() -> OperationContext {
    OperationContext::with_deadline(Instant::now() + Duration::from_secs(3))
}

#[test]
fn trait_registry_stream_ranges_and_capabilities() {
    let (_dir, context) = normal();
    let id = BackendId::new("rclone").unwrap();
    let backend = Arc::new(RcloneBackend::new(id.clone(), context, "crypt:root".into()).unwrap());
    let mut registry = BackendRegistry::new();
    registry.register(backend.clone()).unwrap();
    let reference = ObjectRef::new(id, ObjectKey::new("object").unwrap());
    let resolved = registry.resolve(&reference).unwrap();
    let ctx = OperationContext::none();
    assert_eq!(resolved.stat(&ctx, reference.key()).unwrap().size, 6);
    let mut bytes = Vec::new();
    let receipt = resolved
        .read(
            &ctx,
            reference.key(),
            &ReadRange::new(1, 3).unwrap(),
            &mut bytes,
        )
        .unwrap();
    assert_eq!(bytes, b"bcd");
    assert_eq!(receipt.bytes_read, 3);
    assert_eq!(
        resolved.read_all(&ctx, reference.key(), Some(2)).unwrap(),
        b"ab"
    );
    let caps = resolved.capabilities();
    assert!(!caps.conditional_update.is_supported());
    assert!(!caps.copy_same_backend.is_supported());
    assert_eq!(
        resolved
            .rename(&ctx, reference.key(), reference.key())
            .unwrap_err()
            .kind(),
        StorageErrorKind::Unsupported
    );
}

#[test]
fn exact_legacy_unicode_backslash_colon_and_native_paths_with_spaces() {
    let (_dir, context) = normal();
    let address = "echo:한글\\folder:part %20/../exact";
    assert_eq!(
        context
            .read_all_raw(&OperationContext::none(), address, None)
            .unwrap(),
        address.as_bytes()
    );
}

#[test]
fn stat_classification_never_turns_auth_config_or_json_errors_into_missing() {
    let (_dir, context) = normal();
    let ctx = OperationContext::none();
    for (key, kind) in [
        ("missing", StorageErrorKind::NotFound),
        ("auth", StorageErrorKind::Authentication),
        ("denied", StorageErrorKind::PermissionDenied),
        ("badconfig", StorageErrorKind::Other),
        ("malformed", StorageErrorKind::InvalidInput),
    ] {
        let error = context.stat_raw(&ctx, &format!("crypt:{key}")).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert!(!error.to_string().contains("secret-marker"));
    }
}

#[test]
fn crypt_policy_fresh_config_and_contexts_cannot_share_stale_cache() {
    let (dir, context) = normal();
    let ctx = OperationContext::none();
    context
        .write_raw(
            &ctx,
            "crypt:object",
            &mut &b"first"[..],
            Some(5),
            &WriteOptions::default(),
        )
        .unwrap();
    let path = dir.path().join("config file.json");
    std::fs::write(&path, r#"{"crypt":{"type":"local"}}"#).unwrap();
    assert_eq!(
        context
            .write_raw(
                &ctx,
                "crypt:object",
                &mut &b"second"[..],
                Some(6),
                &WriteOptions::default()
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::InvalidInput
    );
    assert_eq!(
        std::fs::read(dir.path().join("config file.json.written")).unwrap(),
        b"first"
    );
    let (_other, other) = normal();
    other.ensure_crypt(&ctx, "crypt:object").unwrap();
}

#[test]
fn disabled_malformed_override_and_conditional_writes_fail_closed() {
    for config in [
        r#"{"crypt":{"type":"crypt","no_data_encryption":true}}"#,
        r#"{"crypt":{"type":"crypt","no_data_encryption":"nonsense"}}"#,
        r#"{"crypt":{"type":"local"}}"#,
    ] {
        let (dir, context) = setup(config);
        assert!(context
            .write_raw(
                &OperationContext::none(),
                "crypt:object",
                &mut &b"x"[..],
                None,
                &WriteOptions::default()
            )
            .is_err());
        assert!(!dir.path().join("config file.json.written").exists());
    }
    let (_dir, mut context) = normal();
    context
        .environment
        .push(("RCLONE_CRYPT_NO_DATA_ENCRYPTION".into(), "true".into()));
    assert!(context
        .ensure_crypt(&OperationContext::none(), "crypt:x")
        .is_err());
    assert_eq!(
        context
            .write_raw(
                &OperationContext::none(),
                "crypt:x",
                &mut &b"x"[..],
                None,
                &WriteOptions {
                    overwrite: false,
                    expected_version: None
                }
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::Unsupported
    );
}

#[test]
fn lost_mutation_acknowledgement_is_not_retryable() {
    let (dir, context) = normal();
    let error = context
        .write_raw(
            &OperationContext::none(),
            "crypt:lost",
            &mut &b"committed"[..],
            Some(9),
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnknownOutcome);
    assert!(!error.is_retriable());
    assert_eq!(
        std::fs::read(dir.path().join("config file.json.written")).unwrap(),
        b"committed"
    );
}

#[test]
fn deadline_kills_stalled_stdout_and_stdin_on_each_os() {
    let (_dir, context) = normal();
    let start = Instant::now();
    assert_eq!(
        context
            .read_all_raw(&deadline(), "crypt:hang", None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::Timeout
    );
    let bytes = vec![42; 1024 * 1024];
    let mut command = context.command(&["rcat".into(), "--".into(), "crypt:hang".into()]);
    assert_eq!(
        process::run(
            &mut command,
            &deadline(),
            Some(&mut bytes.as_slice()),
            &mut io::sink(),
            true
        )
        .unwrap_err()
        .kind(),
        StorageErrorKind::UnknownOutcome
    );
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn bounded_output_and_stderr_flood_do_not_deadlock() {
    let (_dir, context) = normal();
    // Drain correctness, not a 3-second performance requirement on busy CI hosts.
    let budget = || OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
    assert_eq!(
        context
            .read_all_raw(&budget(), "crypt:flood", None)
            .unwrap(),
        b"abcdef"
    );
    let mut limited = BoundedVec {
        bytes: Vec::new(),
        limit: 1024,
    };
    assert_eq!(
        context
            .read_raw(&budget(), "crypt:oversized", None, &mut limited)
            .unwrap_err()
            .kind(),
        StorageErrorKind::InvalidInput
    );
    assert!(limited.bytes.len() <= 1024);
}

struct FailingReader;
impl Read for FailingReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Other, "secret source marker"))
    }
}
struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Other, "secret sink marker"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn callback_failures_cleanup_and_sanitize() {
    let (_dir, context) = normal();
    // Exercise callback failure after crypt preflight, not startup throughput
    // under parallel fixture load. Deadline behavior has dedicated tests.
    let budget = || OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
    let error = context
        .write_raw(
            &budget(),
            "crypt:object",
            &mut FailingReader,
            None,
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnknownOutcome);
    assert!(!error.to_string().contains("secret"));
    assert!(context
        .read_raw(&budget(), "crypt:oversized", None, &mut FailingWriter)
        .is_err());
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert_eq!(
        context
            .read_all_raw(&OperationContext::with_cancel(flag), "crypt:object", None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::Cancelled
    );
}

#[test]
fn storage_reader_preserves_adapter_error_taxonomy() {
    let exe = executable();
    let reader = crate::storage::reader::StorageReader::rclone(exe.to_str().unwrap());
    assert_eq!(reader.stat("crypt:object").unwrap().size, 6);
    let missing = reader.stat("crypt:missing").unwrap_err();
    assert_eq!(
        missing.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::NotFound
    );
    let auth = reader.stat("crypt:auth").unwrap_err();
    assert_ne!(
        auth.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::NotFound
    );
    assert_eq!(reader.read_metadata("crypt:object").unwrap(), b"abcdef");
}

#[test]
fn storage_services_verify_hash_offsets_and_uploads() {
    let exe = executable();
    let storage = crate::storage::writer::StorageWriter::rclone(exe.to_str().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source file");
    std::fs::write(&source, b"abcdef").unwrap();
    let hash = blake3::hash(b"abcdef").to_hex().to_string();
    let mut shard = crate::storage::writer::descriptor("crypt:object", 6, hash);
    shard.offset = 2;
    storage.write_file(&source, 0, &shard, 3).unwrap();
    storage.write_bytes("crypt:object", b"abcdef", 3).unwrap();
    storage
        .copy_verified(&shard, "crypt:destination", 3)
        .unwrap();
    storage.reader().verify(&shard, true).unwrap();
    assert!(matches!(
        storage.reader().probe(&shard, true),
        crate::models::Probe::Ok
    ));
    let output = dir.path().join("output file");
    std::fs::write(&output, [0u8; 10]).unwrap();
    storage.reader().download(&shard, &output, 2, true).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"\0\0abcdef\0\0");
    storage
        .reader()
        .download(&shard, &output, 2, false)
        .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"abcdef");
}

#[test]
fn range_overdelivery_rejected_before_sink_and_copy_enforces_gate() {
    let (_dir, context) = normal();
    let mut sink = Vec::new();
    let error = context
        .read_raw(
            &deadline(),
            "crypt:oversized",
            Some(&ReadRange::new(0, 1).unwrap()),
            &mut sink,
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidInput);
    assert!(!matches!(error, StorageError::OutputBoundsViolated));
    assert!(sink.is_empty());
    let (dir, context) = setup(r#"{"crypt":{"type":"local"}}"#);
    assert!(context
        .copy_raw(&OperationContext::none(), "source:object", "crypt:object")
        .is_err());
    assert!(!dir.path().join("config file.json.written").exists());
}

#[test]
fn injected_legacy_alias_keeps_exact_address_and_rejects_unknown_key() {
    let (_dir, context) = normal();
    let reader = crate::storage::reader::StorageReader::with_rclone_context(context.clone());
    for raw in [
        "echo:한글\\part:one %20",
        "echo:e\u{301}/../x",
        "echo:é/./x",
    ] {
        let shard = crate::models::Shard {
            index: 0,
            offset: 0,
            size: raw.len() as u64,
            remote: "echo:".into(),
            object: raw.into(),
            blake3: blake3::hash(raw.as_bytes()).to_hex().to_string(),
            kind: crate::models::ShardKind::Data,
            group: 0,
            slot: 0,
        };
        let mut output = Vec::new();
        reader.verified_read(&shard, &mut output).unwrap();
        assert_eq!(output, raw.as_bytes());
    }
    let backend = RcloneBackend::for_legacy_object(
        BackendId::new("exact").unwrap(),
        context,
        "echo:original".into(),
    );
    assert_eq!(
        backend
            .stat(
                &OperationContext::none(),
                &ObjectKey::new("unknown").unwrap()
            )
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
}

#[test]
fn write_service_checks_crypt_policy_even_before_reuse() {
    for config in [
        r#"{"crypt":{"type":"local"}}"#,
        r#"{"crypt":{"type":"crypt","no_data_encryption":true}}"#,
    ] {
        let (dir, context) = setup(config);
        let writer = crate::storage::writer::StorageWriter::synthetic(
            crate::storage::reader::StorageReader::with_rclone_context(context),
        );
        // Fixture returns these exact bytes: reuse would otherwise succeed.
        let err = writer
            .write_bytes("crypt:object", b"abcdef", 3)
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<StorageError>().unwrap().kind(),
            StorageErrorKind::InvalidInput
        );
        assert!(!dir.path().join("config file.json.written").exists());
    }
}

#[test]
fn admin_and_tool_adapter_use_bounded_process_owner() {
    use crate::storage::admin::{BackendAdmin, RcloneAdmin, ToolDiagnostics};
    let (_dir, context) =
        setup(r#"{"drive":{"type":"drive"},"crypt":{"type":"crypt","remote":"drive:folder"}}"#);
    let admin = RcloneAdmin::new(context);
    assert_eq!(admin.version().unwrap(), "rclone v1.fixture");
    assert_eq!(admin.discover().unwrap(), vec!["crypt:"]);
    assert!(admin.probe("crypt:object").is_ok());
    assert!(admin.probe("crypt:empty").is_ok());
    assert!(admin.probe("crypt:missing").is_err());
    let error = admin.probe("crypt:auth").unwrap_err();
    assert!(!format!("{error:#}").contains("secret-marker"));
    let binding = admin.catalog().unwrap().capacity("crypt:path").unwrap();
    assert_eq!(binding.target, "drive:");
    assert!(binding.failure_domain.is_none());
    assert_eq!(admin.quota(&binding.target).free, Some(75));
}

// A process-wide mount scope must be tested in its own process, otherwise parallel
// test operations would intentionally inherit its cancellation token too.
#[test]
fn mount_process_cancellation_inherits_across_threads_and_reaps_children() {
    const CHILD: &str = "RPOOL_TEST_MOUNT_CANCELLATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let module = module_path!().split_once("::").unwrap().1;
        let name = format!(
            "{module}::mount_process_cancellation_inherits_across_threads_and_reaps_children"
        );
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    use crate::storage::traits::ProcessCancellationGuard;
    use std::sync::atomic::{AtomicBool, Ordering};
    let unrelated = OperationContext::none();
    for mutation in [false, true] {
        let (dir, context) = normal();
        let started = dir.path().join("config file.json.started");
        let flag = Arc::new(AtomicBool::new(false));
        let guard = ProcessCancellationGuard::install(flag.clone()).unwrap();
        assert!(ProcessCancellationGuard::install(flag.clone()).is_err());
        let inherited = std::thread::spawn(|| {
            [
                OperationContext::none(),
                OperationContext::with_deadline(Instant::now() + Duration::from_secs(30)),
                OperationContext::with_cancel(Arc::new(AtomicBool::new(false))),
                OperationContext::with_deadline_and_cancel(
                    Instant::now() + Duration::from_secs(30),
                    Arc::new(AtomicBool::new(false)),
                ),
            ]
        })
        .join()
        .unwrap();
        let observed = started.clone();
        let cancel = flag.clone();
        let stopper = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while !observed.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.store(true, Ordering::Release);
        });
        let begin = Instant::now();
        let mut command = context.command(&[
            if mutation {
                "rcat".into()
            } else {
                "cat".into()
            },
            "--".into(),
            "crypt:hang".into(),
        ]);
        let bytes = vec![42; 1024 * 1024];
        let mut source = bytes.as_slice();
        let error = process::run(
            &mut command,
            &inherited[0],
            if mutation { Some(&mut source) } else { None },
            &mut io::sink(),
            mutation,
        )
        .unwrap_err();
        stopper.join().unwrap();
        assert!(
            started.exists(),
            "fixture must actually start before cancellation"
        );
        assert!(begin.elapsed() < Duration::from_secs(10));
        assert_eq!(
            error.kind(),
            if mutation {
                StorageErrorKind::UnknownOutcome
            } else {
                StorageErrorKind::Cancelled
            }
        );
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            let pid = std::fs::read_to_string(&started)
                .unwrap()
                .parse::<i32>()
                .unwrap();
            assert_eq!(
                unsafe { kill(pid, 0) },
                -1,
                "cancelled child must be reaped"
            );
        }
        assert!(inherited.iter().all(OperationContext::is_cancelled));
        let dispatched = AtomicBool::new(false);
        let result = crate::storage::scheduler::run(
            vec![()],
            1,
            1,
            |_| "remote".into(),
            |_| {
                dispatched.store(true, Ordering::Release);
                Ok(())
            },
            |_, _| None,
            |_, result| {
                result?;
                Ok(Vec::new())
            },
        );
        assert!(result.is_err());
        assert!(!dispatched.load(Ordering::Acquire));
        // Cancellation during a long retry-after must wake the queue promptly.
        flag.store(false, Ordering::Release);
        let (retry_started, retry_received) = std::sync::mpsc::channel();
        let cancel = flag.clone();
        let stopper = std::thread::spawn(move || {
            retry_received.recv_timeout(Duration::from_secs(5)).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Release);
        });
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let begin = Instant::now();
        let result = crate::storage::scheduler::run(
            vec![()],
            1,
            2,
            |_| "remote".into(),
            |_| -> anyhow::Result<()> {
                attempts.fetch_add(1, Ordering::Relaxed);
                anyhow::bail!("synthetic retryable error")
            },
            |_, _| {
                retry_started.send(()).unwrap();
                Some(Duration::from_secs(30))
            },
            |_, result| {
                result?;
                Ok(Vec::new())
            },
        );
        stopper.join().unwrap();
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(begin.elapsed() < Duration::from_secs(5));
        // A running mutation can report an uncertain outcome after the
        // scheduler has already noticed cancellation. Never mask that outcome.
        for terminal in [
            crate::storage::error::StorageError::unknown_outcome("synthetic interrupted write"),
            crate::storage::error::StorageError::CorruptData {
                found: "actual".into(),
                expected: "expected".into(),
            },
        ] {
            flag.store(false, Ordering::Release);
            let completed = AtomicBool::new(false);
            let error = crate::storage::scheduler::run(
                vec![()],
                1,
                1,
                |_| "remote".into(),
                |_| -> anyhow::Result<()> {
                    flag.store(true, Ordering::Release);
                    // Allow the scheduler's cancellation poll to fire while
                    // this already-running operation is still unwinding.
                    std::thread::sleep(Duration::from_millis(200));
                    Err(terminal.clone().into())
                },
                |_, _| None,
                |_, result| {
                    completed.store(true, Ordering::Release);
                    result?;
                    Ok(Vec::new())
                },
            )
            .unwrap_err();
            assert!(completed.load(Ordering::Acquire));
            assert_eq!(
                error
                    .downcast_ref::<crate::storage::error::StorageError>()
                    .unwrap()
                    .kind(),
                terminal.kind(),
            );
        }
        assert!(!unrelated.is_cancelled());
        drop(guard);
        assert!(
            inherited[0].is_cancelled(),
            "existing operations retain ownership"
        );
        assert!(!OperationContext::none().is_cancelled());
    }
}
