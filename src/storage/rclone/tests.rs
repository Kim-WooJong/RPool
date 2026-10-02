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
                    expected_version: None,
                    defer_hash_check: false,
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
        Err(io::Error::other("secret source marker"))
    }
}
struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("secret sink marker"))
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

#[test]
fn provider_rejected_mutations_retry_with_backoff_then_report_retriable() {
    let (dir, context) = normal();
    let ctx = OperationContext::none();
    let counter = dir.path().join("config file.json.busy");
    // Rejected twice, then performed: the retry is invisible to the caller.
    context.delete_raw(&ctx, "crypt:busy-twice").unwrap();
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "3");
    std::fs::remove_file(&counter).unwrap();
    context
        .copy_raw(&ctx, "crypt:source", "crypt:busy-twice")
        .unwrap();
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "3");
    std::fs::remove_file(&counter).unwrap();
    // Always rejected: 1 + 3 attempts, then a retriable RateLimited.
    let error = context.delete_raw(&ctx, "crypt:busyforever").unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::RateLimited);
    assert!(error.is_retriable());
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "4");
    std::fs::remove_file(&counter).unwrap();
    // An upload's source is consumed: no in-layer retry, but retriable.
    let error = context
        .write_raw(
            &ctx,
            "crypt:busyforever",
            &mut &b"data"[..],
            Some(4),
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::RateLimited);
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "1");
    // Anything else stays an unknown outcome and is attempted once.
    let error = context.delete_raw(&ctx, "crypt:unsure").unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnknownOutcome);
}

#[test]
fn rejected_write_signals_are_exact() {
    for rejected in [
        "ERROR : f: Failed to copy: copy failed: from_write/too_many_write_operations/..",
        "upload failed: too_many_requests/.",
        "googleapi: Error 403: User Rate Limit Exceeded, userRateLimitExceeded",
        "googleapi: Error 403: Rate Limit Exceeded, rateLimitExceeded",
        "HTTP error 429 (429 Too Many Requests) returned body: \"\"",
        "failed: status code 429",
        "Error 429: slow down",
        "Post request rcat error: Update mkParentDir failed: Locked: 423 Locked",
    ] {
        assert!(
            process::mutation_rejected(rejected.as_bytes()),
            "{rejected}"
        );
    }
    for unsure in [
        "Failed to copy: connection reset by peer",
        "context deadline exceeded",
        "error 4290 at offset",
        "object 4290 bytes",
        "HTTP error 500 (500 Internal Server Error)",
        "status code 4291",
        "file is locked by another process",
        "",
    ] {
        assert!(!process::mutation_rejected(unsure.as_bytes()), "{unsure}");
    }
}

#[test]
fn write_base_follows_wrappers_to_the_storage_namespace() {
    let config: Value = serde_json::from_str(
        r#"{"dropbox_1":{"type":"dropbox"},"dropbox_1_crypt":{"type":"crypt","remote":"dropbox_1:pool"},
            "alias_c":{"type":"alias","remote":"dropbox_1_crypt:x"},"drime":{"type":"drime"},
            "drime_crypt":{"type":"crypt","remote":"drime:"},"loop":{"type":"alias","remote":"loop:"}}"#,
    )
    .unwrap();
    assert_eq!(
        write_base(&config, "dropbox_1_crypt"),
        ("dropbox_1".into(), true)
    );
    assert_eq!(write_base(&config, "alias_c"), ("dropbox_1".into(), true));
    assert_eq!(write_base(&config, "dropbox_1"), ("dropbox_1".into(), true));
    assert_eq!(write_base(&config, "drime_crypt"), ("drime".into(), false));
    assert_eq!(write_base(&config, "unknown"), ("unknown".into(), false));
    assert_eq!(write_base(&config, "loop"), ("loop".into(), false));
}

#[test]
fn a_fake_rclone_disables_the_daemon_and_reads_use_subprocesses() {
    let (_dir, mut context) = normal();
    context.allow_daemon_for_test();
    let started = Instant::now();
    assert!(daemon::get(&context).is_none());
    assert!(started.elapsed() < Duration::from_secs(5));
    let ctx = OperationContext::none();
    assert_eq!(context.stat_raw(&ctx, "crypt:object").unwrap().size, 6);
    let mut bytes = Vec::new();
    context
        .read_raw(
            &ctx,
            "crypt:object",
            Some(&ReadRange::new(1, 3).unwrap()),
            &mut bytes,
        )
        .unwrap();
    assert_eq!(bytes, b"bcd");
    assert!(daemon::get(&context).is_none(), "permanent fallback");
}

#[test]
fn traffic_counters_count_exact_bytes_outcomes_and_retries() {
    use super::traffic::snapshot;
    let (dir, mut context) = setup(
        r#"{"metw":{"type":"crypt"},"metr":{"type":"crypt"},"metb":{"type":"crypt"},"metcrypt":{"type":"crypt"}}"#,
    );
    let ctx = deadline();
    // Upload: stdin bytes are sent, acknowledged on exit 0. No parent folder
    // at the remote root, so no mkdir.
    context
        .write_raw(
            &ctx,
            "metw:obj",
            &mut &[7u8; 10][..],
            Some(10),
            &WriteOptions::default(),
        )
        .unwrap();
    let t = snapshot("metw");
    assert_eq!((t.sent_bytes, t.acked_bytes, t.received_bytes), (10, 10, 0));
    assert_eq!((t.ok_ops, t.failed_ops, t.active_uploads), (1, 0, 0));
    assert!(t.last_ok_unix.is_some() && t.upload_rate_10s >= 0.0);
    // A lost acknowledgement: sent but not acked, one failure.
    context
        .write_raw(
            &ctx,
            "metw:lost",
            &mut &[7u8; 4][..],
            Some(4),
            &WriteOptions::default(),
        )
        .unwrap_err();
    let t = snapshot("metw");
    assert_eq!((t.sent_bytes, t.acked_bytes, t.failed_ops), (14, 10, 1));
    assert!(t.last_error.unwrap().contains("did not acknowledge"));
    // Reads (subprocess): stdout bytes are received.
    let mut sink = Vec::new();
    context.read_raw(&ctx, "metr:x", None, &mut sink).unwrap();
    let range = ReadRange::new(2, 3).unwrap();
    context
        .read_raw(&ctx, "metr:x", Some(&range), &mut sink)
        .unwrap();
    assert_eq!(sink, b"abcdefcde");
    context.stat_raw(&ctx, "metr:x").unwrap();
    let t = snapshot("metr");
    // Two reads plus the stat's lsjson output (24 bytes).
    assert_eq!(
        (t.received_bytes, t.ok_ops, t.active_downloads),
        (9 + 24, 3, 0)
    );
    // A missing object is a definite answer, not a failure; auth is.
    context.stat_raw(&ctx, "metr:missing").unwrap_err();
    context
        .read_raw(&ctx, "metr:auth", None, &mut sink)
        .unwrap_err();
    let t = snapshot("metr");
    assert_eq!((t.ok_ops, t.failed_ops), (4, 1));
    let error = t.last_error.unwrap();
    assert!(
        error.contains("authentication") && !error.contains("secret-marker"),
        "{error}"
    );
    assert!(t.failing_since_unix.is_some());
    // Provider-rejected mutations: each backoff is a retry of one operation.
    let counter = dir.path().join("config file.json.busy");
    context.delete_raw(&ctx, "metb:busy-twice").unwrap();
    std::fs::remove_file(&counter).unwrap();
    context
        .delete_raw(&OperationContext::none(), "metb:busyforever")
        .unwrap_err();
    let t = snapshot("metb");
    assert_eq!((t.ok_ops, t.failed_ops, t.retries), (1, 1, 2 + 3));
    // Native crypt base traffic counts for its crypt remote.
    context.attribute_traffic("metbase", "metcrypt");
    context
        .write_ungated(
            &ctx,
            "metbase:obj",
            &mut &[1u8; 5][..],
            None,
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(snapshot("metcrypt").acked_bytes, 5);
    assert_eq!(snapshot("metbase"), super::traffic::Traffic::default());
    // Admin calls without an address are not metered.
    context.config_dump(&ctx).unwrap();
}

#[test]
fn provider_upload_limits_map_to_their_own_wait_state() {
    let limit = [
        "ERROR : gd: Received upload limit error: googleapi: Error 403: User rate limit exceeded., userRateLimitExceeded",
        "Failed to copy: googleapi: Error 403: User rate limit exceeded., userRateLimitExceeded",
        "googleapi: Error 403: Daily Limit Exceeded, dailyLimitExceeded",
    ];
    for text in limit {
        assert!(process::upload_limit_reported(text.as_bytes()), "{text}");
    }
    for other in [
        // The short-term per-user limit stays an ordinary, retried rate limit.
        "googleapi: Error 403: User Rate Limit Exceeded, userRateLimitExceeded",
        "Received upload limit error: googleapi: Error 403: The user's Drive storage quota has been exceeded., storageQuotaExceeded",
        "too_many_write_operations",
        "",
    ] {
        assert!(!process::upload_limit_reported(other.as_bytes()), "{other}");
    }
    let classified = |text: &str| {
        let status = Command::new(if cfg!(windows) { "cmd" } else { "false" })
            .args(if cfg!(windows) {
                &["/C", "exit 1"][..]
            } else {
                &[][..]
            })
            .status()
            .unwrap();
        process::classify(status, text.as_bytes(), true)
    };
    let error = classified(limit[0]);
    assert!(process::is_upload_limit(&error) && error.is_retriable());
    assert!(error.retry_after().is_none());
    let ordinary =
        classified("googleapi: Error 403: User Rate Limit Exceeded, userRateLimitExceeded");
    assert_eq!(ordinary.kind(), StorageErrorKind::RateLimited);
    assert!(!process::is_upload_limit(&ordinary));
}

#[test]
fn write_account_reports_the_bottom_backend_type() {
    let config: Value = serde_json::from_str(
        r#"{"gd":{"type":"drive"},"gd_crypt":{"type":"crypt","remote":"gd:pool"},
            "alias_c":{"type":"alias","remote":"gd_crypt:x"},"broken":{"type":"crypt"}}"#,
    )
    .unwrap();
    assert_eq!(
        write_account(&config, "alias_c"),
        ("gd".into(), "drive".into())
    );
    assert_eq!(write_account(&config, "gd"), ("gd".into(), "drive".into()));
    assert_eq!(
        write_account(&config, "broken"),
        ("broken".into(), "crypt".into())
    );
    assert_eq!(
        write_account(&config, "missing"),
        ("missing".into(), String::new())
    );
}
