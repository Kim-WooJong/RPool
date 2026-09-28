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
use std::time::Duration;

pub(super) const CHUNK: usize = 64 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;
const POLL: Duration = Duration::from_millis(10);

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
fn io_error() -> StorageError {
    StorageError::TransientIo {
        detail: "rclone stream I/O failed".into(),
    }
}
fn sink_error(error: std::io::Error) -> StorageError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput
    ) {
        StorageError::invalid_input("rclone output bounds violated")
    } else {
        io_error()
    }
}

struct ChildGuard(Arc<Mutex<Child>>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn classify(status: ExitStatus, stderr: &[u8], mutation: bool) -> StorageError {
    if mutation {
        return StorageError::unknown_outcome("rclone mutation did not acknowledge success");
    }
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if text.contains("unauthorized")
        || text.contains("authentication")
        || text.contains("invalid_grant")
    {
        StorageError::Authentication {
            detail: "rclone authentication failed".into(),
        }
    } else if text.contains("permission denied")
        || text.contains("access denied")
        || text.contains("forbidden")
    {
        StorageError::PermissionDenied {
            path: "rclone object".into(),
        }
    } else if matches!(status.code(), Some(3 | 4)) {
        // Documented missing exits, after explicit auth/permission evidence.
        StorageError::not_found("rclone object")
    } else if text.contains("429") || text.contains("rate limit") {
        StorageError::RateLimited {
            retry_after: None,
            detail: "rclone rate limited".into(),
        }
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
pub(super) fn run(
    command: &mut Command,
    ctx: &OperationContext,
    mut source: Option<&mut dyn Read>,
    sink: &mut dyn Write,
    mutation: bool,
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
                    if check(ctx).is_err() {
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
                    check(ctx)?;
                    stdin.write_all(&buffer[..n]).map_err(|_| io_error())?;
                    total = total.checked_add(n as u64).ok_or_else(io_error)?;
                }
                drop(stdin);
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
                    check(ctx)?;
                    sink.write_all(&buffer[..n]).map_err(sink_error)?;
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
        match result {
            Ok((total, status)) if status.success() => Ok(total),
            Ok((_, status)) => Err(classify(status, &stderr, mutation)),
            Err(_) if mutation => Err(StorageError::unknown_outcome(
                "rclone mutation interrupted after spawn",
            )),
            Err(error) => Err(check(ctx).err().unwrap_or(error)),
        }
    })
}
