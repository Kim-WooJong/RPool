//! Portable direct-child runner. No shell, platform-specific pipe APIs or borrowed
//! callback Send bounds. Supervisor kills a stalled child on deadline/cancellation.
//! Callbacks themselves must cooperate; descendant process trees are not managed.
use crate::storage::error::StorageError;
use crate::storage::traits::OperationContext;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::stall::Stall;

/// Pipe read/write buffer size (bytes) for stdin/stdout streaming; also the pacing/metering granularity.
pub(super) const CHUNK: usize = 64 * 1024;
/// Most stderr bytes kept for error classification; the rest is drained and dropped.
const STDERR_LIMIT: usize = 16 * 1024;
/// Supervisor polling interval; also the base for cancellation polling in `limit` and `pacer`.
pub(super) const POLL: Duration = Duration::from_millis(10);

/// Fails with `Cancelled` or `Timeout` when `ctx` was cancelled or its deadline passed.
/// Polled throughout the runner, the caps in `limit` and the `pacer` waits.
pub(super) fn check(ctx: &OperationContext) -> Result<(), StorageError> {
    if ctx.is_cancelled() {
        return Err(StorageError::Cancelled {
            detail: "rclone operation cancelled".into(),
        });
    }
    if ctx.deadline_passed() {
        return Err(StorageError::Timeout {
            detail: "rclone deadline elapsed".into(),
        });
    }
    Ok(())
}
/// Generic retriable stream failure; never carries OS error text.
pub(super) fn io_error() -> StorageError {
    StorageError::TransientIo {
        detail: "rclone stream I/O failed".into(),
    }
}
/// Maps a sink write error: admin output cap overflow -> `OutputBoundsViolated`,
/// invalid data/input -> invalid input, anything else -> transient I/O.
pub(super) fn sink_error(error: std::io::Error) -> StorageError {
    if error
        .get_ref()
        .is_some_and(|source| source.is::<super::AdminOutputCap>())
    {
        StorageError::OutputBoundsViolated
    } else if matches!(
        error.kind(),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput
    ) {
        StorageError::invalid_input("rclone output bounds violated")
    } else {
        io_error()
    }
}

/// Kills and reaps the child on drop, so an unwind or early return never leaves rclone running.
struct ChildGuard(Arc<Mutex<Child>>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Provider answers that reject a write before performing it. Only these exact
/// signals make a failed mutation retriable; a `429` must stand alone.
/// WebDAV `423 Locked` refuses the request before any byte is written (a
/// briefly locked resource or parent folder, e.g. while sibling uploads run).
const REJECTED_WRITE_SIGNALS: &[&str] = &[
    "423 locked",
    "too_many_write_operations",
    "too_many_requests",
    "ratelimitexceeded", // also userRateLimitExceeded
    "too many requests",
    "http 429",
    "http error 429",
    "error 429",
    "status 429",
    "status code 429",
    "statuscode 429",
    "statuscode=429",
    "code: 429",
    "code 429",
];

/// Whether a failed mutation's stderr carries a definite "rejected, not
/// performed" rate-limit signal. The stderr itself is never exposed.
pub(super) fn mutation_rejected(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    REJECTED_WRITE_SIGNALS.iter().any(|signal| {
        text.match_indices(signal).any(|(at, _)| {
            !signal.ends_with(|c: char| c.is_ascii_digit())
                || !text.as_bytes()[at + signal.len()..]
                    .first()
                    .is_some_and(u8::is_ascii_digit)
        })
    })
}

/// Exact detail of an upload the provider refused for its upload limit.
pub(super) const UPLOAD_LIMIT_DETAIL: &str =
    "rclone upload refused by the provider's upload limit (not performed)";

/// Whether a failed upload's stderr says the account may not upload more for
/// now (a daily/rolling limit, not a short burst limit and not a full disk):
/// - `Received upload limit error` is rclone's report of Google Drive's
///   upload limit under `--drive-stop-on-upload-limit` (rclone drive backend);
/// - `User rate limit exceeded.` (this exact spelling) is Drive's message for
///   that limit when the flag is absent; the short-term per-user rate limit is
///   spelled `User Rate Limit Exceeded` and stays an ordinary rate limit;
/// - `dailyLimitExceeded` is Google's daily API quota.
///
/// Storage-quota answers (`quotaExceeded`, `storageQuotaExceeded`) are not
/// upload limits: waiting does not free space.
pub(super) fn upload_limit_reported(stderr: &[u8]) -> bool {
    let raw = String::from_utf8_lossy(stderr);
    let text = raw.to_ascii_lowercase();
    if text.contains("quotaexceeded") || text.contains("storage quota") {
        return false;
    }
    text.contains("received upload limit error")
        || raw.contains("User rate limit exceeded.")
        || text.contains("dailylimitexceeded")
}

/// Whether `error` is a provider upload-limit refusal (see above).
pub(super) fn is_upload_limit(error: &StorageError) -> bool {
    matches!(error, StorageError::RateLimited { detail, .. } if detail == UPLOAD_LIMIT_DETAIL)
}

/// Explicit authentication/permission evidence in an rclone error message.
pub(super) fn denial(text: &str) -> Option<StorageError> {
    let text = text.to_ascii_lowercase();
    if text.contains("unauthorized")
        || text.contains("authentication")
        || text.contains("invalid_grant")
    {
        Some(StorageError::Authentication {
            detail: "rclone authentication failed".into(),
        })
    } else if text.contains("permission denied")
        || text.contains("access denied")
        || text.contains("forbidden")
    {
        Some(StorageError::PermissionDenied {
            path: "rclone object".into(),
        })
    } else {
        None
    }
}

/// A provider rate limit: HTTP 429 as a standalone number (not part of a
/// longer number such as a byte count or an id), or the words "rate limit" /
/// "too many requests".
pub(super) fn rate_limited(text: &str) -> Option<StorageError> {
    let text = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let standalone_429 = text.match_indices("429").any(|(at, _)| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + 3).copied();
        !before.is_some_and(|b| b.is_ascii_alphanumeric())
            && !after.is_some_and(|b| b.is_ascii_alphanumeric())
    });
    (standalone_429 || text.contains("rate limit") || text.contains("too many requests")).then(
        || StorageError::RateLimited {
            retry_after: None,
            detail: "rclone rate limited".into(),
        },
    )
}

/// Turns a failed rclone exit into a [`StorageError`] from its exit code and stderr.
/// Mutations are `RateLimited` only on explicit provider rejection evidence, else
/// unknown outcome (the write may have happened); reads map denial, missing (exit 3/4),
/// rate limits, timeouts and exit 5 (I/O). Used by every subprocess call.
pub(super) fn classify(status: ExitStatus, stderr: &[u8], mutation: bool) -> StorageError {
    if mutation {
        if upload_limit_reported(stderr) {
            return StorageError::RateLimited {
                retry_after: None,
                detail: UPLOAD_LIMIT_DETAIL.into(),
            };
        }
        if mutation_rejected(stderr) {
            return StorageError::RateLimited {
                retry_after: None,
                detail: "rclone write rejected by provider rate limit (not performed)".into(),
            };
        }
        return StorageError::unknown_outcome("rclone mutation did not acknowledge success");
    }
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if let Some(error) = denial(&text) {
        error
    } else if matches!(status.code(), Some(3 | 4)) {
        // Documented missing exits, after explicit auth/permission evidence.
        StorageError::not_found("rclone object")
    } else if let Some(error) = rate_limited(&text) {
        error
    } else if text.contains("timeout") || text.contains("timed out") {
        StorageError::Timeout {
            detail: "rclone timed out".into(),
        }
    } else if status.code() == Some(5) {
        io_error()
    } else {
        StorageError::Other {
            detail: format!("rclone failed (exit {:?})", status.code()),
        }
    }
}

/// Bounded pipe buffers. Captured stderr is never exposed in public error text.
#[cfg(test)]
pub(super) fn run(
    command: &mut Command,
    ctx: &OperationContext,
    source: Option<&mut dyn Read>,
    sink: &mut dyn Write,
    mutation: bool,
) -> Result<u64, StorageError> {
    run_metered(command, ctx, source, sink, mutation, None)
}

/// `run` that counts each stdin chunk rclone accepted as sent and each
/// stdout chunk as received bytes of `meter`.
pub(super) fn run_metered(
    command: &mut Command,
    ctx: &OperationContext,
    source: Option<&mut dyn Read>,
    sink: &mut dyn Write,
    mutation: bool,
    meter: Option<&super::traffic::Op>,
) -> Result<u64, StorageError> {
    run_supervised(command, ctx, source, sink, mutation, meter, None)
}

/// A streamed upload (`rcat`): [`run_metered`] plus stall detection
/// (`stall`). A stalled rclone is killed and reported as a retriable
/// `Timeout`, never as an unknown outcome.
pub(super) fn run_upload(
    command: &mut Command,
    ctx: &OperationContext,
    source: &mut dyn Read,
    meter: Option<&super::traffic::Op>,
) -> Result<u64, StorageError> {
    let stall = Stall::upload();
    run_supervised(
        command,
        ctx,
        Some(source),
        &mut std::io::sink(),
        true,
        meter,
        Some(&stall),
    )
}

/// Detail of the retriable error of a stalled upload.
pub(super) const STALLED_DETAIL: &str = "rclone upload stalled; stopped so it can be re-sent";

/// Shared body of [`run_metered`] and [`run_upload`]: spawns `command`, streams
/// `source` to stdin or stdout to `sink`, while a supervisor thread kills the child
/// on cancellation, deadline or upload stall; returns bytes streamed or the classified error.
fn run_supervised(
    command: &mut Command,
    ctx: &OperationContext,
    mut source: Option<&mut dyn Read>,
    sink: &mut dyn Write,
    mutation: bool,
    meter: Option<&super::traffic::Op>,
    stall: Option<&Stall>,
) -> Result<u64, StorageError> {
    check(ctx)?;
    command
        .stdin(if source.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if source.is_some() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; argv stays native.
    }
    let mut child = command.spawn().map_err(|_| StorageError::Other {
        detail: "could not start rclone executable".into(),
    })?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take().expect("piped stderr");
    let shared = Arc::new(Mutex::new(child));
    thread::scope(|scope| {
        // Also covers thread-creation panics before either worker exists.
        let guard = ChildGuard(shared.clone());
        let stopped = Arc::new(AtomicBool::new(false));
        let supervisor_child = shared.clone();
        let stop = stopped.clone();
        scope.spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if let Ok(mut child) = supervisor_child.lock() {
                    if check(ctx).is_err() || stall.is_some_and(|s| s.expired(Instant::now())) {
                        let _ = child.kill();
                        return;
                    }
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                }
                thread::sleep(POLL);
            }
        });
        let errors = scope.spawn(move || {
            let mut stderr = stderr;
            let mut retained = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let keep = n.min(STDERR_LIMIT - retained.len());
                        retained.extend_from_slice(&buffer[..keep]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            retained
        });
        // Guard is inside scope: unwind kills/reaps before scope joins pipe workers.
        // rclone may reject a write and exit before reading all of stdin; its
        // exit status and stderr then decide, not the broken pipe.
        let mut stdin_broken = false;
        let result = (|| {
            let mut total = 0u64;
            let mut buffer = [0u8; CHUNK];
            if let Some(input) = source.as_mut() {
                let mut stdin = stdin.expect("piped stdin");
                loop {
                    check(ctx)?;
                    let n = match input.read(&mut buffer) {
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        result => result.map_err(|_| io_error())?,
                    };
                    if n == 0 {
                        break;
                    }
                    if let Some(meter) = meter {
                        meter.throttle(ctx, n as u64);
                    }
                    check(ctx)?;
                    // Throttle time is not a stall; only a blocked write is.
                    if let Some(stall) = stall {
                        stall.progress();
                    }
                    match stdin.write_all(&buffer[..n]) {
                        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                            stdin_broken = true;
                            break;
                        }
                        result => result.map_err(|_| io_error())?,
                    }
                    if let Some(stall) = stall {
                        stall.progress();
                    }
                    if let Some(meter) = meter {
                        meter.sent(n as u64);
                    }
                    total = total.checked_add(n as u64).ok_or_else(io_error)?;
                }
                drop(stdin);
                if let Some(stall) = stall {
                    stall.closed(total);
                }
            } else {
                let mut stdout = stdout.expect("piped stdout");
                loop {
                    check(ctx)?;
                    let n = match stdout.read(&mut buffer) {
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        result => result.map_err(|_| io_error())?,
                    };
                    if n == 0 {
                        break;
                    }
                    if let Some(meter) = meter {
                        meter.throttle(ctx, n as u64);
                    }
                    check(ctx)?;
                    sink.write_all(&buffer[..n]).map_err(sink_error)?;
                    if let Some(meter) = meter {
                        meter.received(n as u64);
                    }
                    total = total.checked_add(n as u64).ok_or_else(io_error)?;
                }
            }
            loop {
                check(ctx)?;
                let status = shared
                    .lock()
                    .map_err(|_| io_error())?
                    .try_wait()
                    .map_err(|_| io_error())?;
                if let Some(status) = status {
                    return Ok((total, status));
                }
                thread::sleep(POLL);
            }
        })();
        if result.is_err() {
            if let Ok(mut child) = shared.lock() {
                let _ = child.kill();
            }
        }
        drop(guard); // Reap before joining stderr, also on callback failures.
        stopped.store(true, Ordering::Release);
        let stderr = errors.join().map_err(|_| io_error())?;
        let stalled = stall.is_some_and(Stall::fired);
        match result {
            Ok((_, status)) if stdin_broken && status.success() => Err(
                StorageError::unknown_outcome("rclone stopped reading the upload stream"),
            ),
            Ok((total, status)) if status.success() => Ok(total),
            // Killed by the stall deadline: the shard is re-sent by the caller.
            _ if stalled => Err(StorageError::Timeout {
                detail: STALLED_DETAIL.into(),
            }),
            Ok((_, status)) => Err(classify(status, &stderr, mutation)),
            Err(_) if mutation => Err(StorageError::unknown_outcome(
                "rclone mutation interrupted after spawn",
            )),
            Err(error) => Err(check(ctx).err().unwrap_or(error)),
        }
    })
}

#[cfg(all(test, unix))]
mod stall_tests {
    use super::*;
    use crate::storage::error::StorageErrorKind;
    use std::time::Instant;

    /// Runs `script` as a fake rclone upload with a 300 ms stall floor.
    fn upload(script: &str, bytes: usize) -> (Result<u64, StorageError>, Duration) {
        let stall = Stall::new(Duration::from_millis(300), 1024 * 1024);
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let data = vec![7u8; bytes];
        let started = Instant::now();
        let result = run_supervised(
            &mut command,
            &OperationContext::none(),
            Some(&mut data.as_slice()),
            &mut std::io::sink(),
            true,
            None,
            Some(&stall),
        );
        (result, started.elapsed())
    }

    #[test]
    fn a_stalled_upload_is_killed_and_reported_retriable() {
        // Accepts every byte, then hangs like a stalled provider connection.
        let (result, took) = upload("cat >/dev/null; exec sleep 30", 64 * 1024);
        let error = result.unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Timeout);
        assert!(error.is_retriable(), "the scheduler re-sends the shard");
        assert!(
            took < Duration::from_secs(10),
            "killed, not waited: {took:?}"
        );
        // Never reads stdin: feeding makes no progress.
        let (result, took) = upload("exec sleep 30", 4 * 1024 * 1024);
        let error = result.unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::Timeout);
        assert!(took < Duration::from_secs(10), "{took:?}");
    }

    #[test]
    fn a_healthy_upload_and_a_real_failure_are_unchanged() {
        let (result, _) = upload("cat >/dev/null", 1024 * 1024);
        assert_eq!(result.unwrap(), 1024 * 1024);
        // A mutation that fails without a stall keeps its unknown outcome.
        let (result, _) = upload("cat >/dev/null; exit 1", 1024);
        assert_eq!(result.unwrap_err().kind(), StorageErrorKind::UnknownOutcome);
    }
}

#[cfg(test)]
mod rate_limit_tests {
    #[test]
    fn only_a_standalone_429_or_rate_limit_words_count() {
        let hit = |t: &str| super::rate_limited(t).is_some();
        assert!(hit("HTTP error 429 (429 Too Many Requests)"));
        assert!(hit("status=429"));
        assert!(hit("Rate limit exceeded"));
        assert!(hit("too many requests"));
        assert!(!hit("wrote 4290000 bytes"));
        assert!(!hit("object a429b not found"));
        assert!(!hit("failed: 14291 items"));
    }
}
