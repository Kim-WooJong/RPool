//! Persistent `rclone rcd` transport for reads and small uploads.
//!
//! Every rclone subprocess pays the backend's cold start again (tens of
//! seconds on some providers); one long-lived daemon per (executable, config
//! selection, environment) keeps backends warm. Lifecycle:
//! - started lazily on the first read, with exactly the global arguments and
//!   environment of [`RcloneContext::command`] (minus `RCLONE_RC_*`, which only
//!   configure the daemon itself);
//! - listens on a unix socket inside a fresh 0700 temp dir (other users cannot
//!   connect) or, on Windows, on 127.0.0.1 with random per-process HTTP Basic
//!   credentials passed through the environment (never the command line);
//! - replaced when the rclone config file changes (size/mtime), because the
//!   subprocess path re-reads it on every call;
//! - if it dies it is restarted once; after that, or when it never becomes
//!   ready (e.g. a fake test rclone), the key falls back to subprocesses for
//!   the rest of the process;
//! - killed by [`ShutdownGuard`] when `main` returns; on Ctrl-C the terminal
//!   delivers SIGINT to the daemon too (same process group). A daemon left by
//!   a crashed RPool is killed by the next RPool only when its owner pid is
//!   gone and the live process's command line still names that daemon's own
//!   socket (unix; Windows orphans are not swept).
//!
//! Routing rule: a definite answer (not found, authentication, permission,
//! rate limit, cancellation, output bounds) is returned as is; anything else
//! ([`Failure::Fallback`]) makes the caller repeat that read via subprocess.
//!
//! Uploads of small objects ([`DAEMON_UPLOAD_MAX`], see `write`) also come
//! here (`operations/uploadfile`): a subprocess per shard pays process start,
//! backend set-up and a new TLS connection for every request, which dominates
//! small shards; the daemon keeps them warm and paces one account's requests
//! in one place. Other mutations (delete, move, copy, large uploads) still
//! use subprocesses.
use super::http::{self, Endpoint, HttpError};
use super::{process, remote_name, BoundedVec, ConfigSelection, RcloneContext, ADMIN_LIMIT};
use crate::storage::error::StorageError;
use crate::storage::traits::OperationContext;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a freshly started daemon may take to answer `rc/noop`.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on non-200 response bodies read for error classification.
const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// Why a daemon read did not succeed; decides whether the caller retries via subprocess.
pub(super) enum Failure {
    /// The daemon's answer stands; do not retry via subprocess.
    Definite(StorageError),
    /// No trustworthy answer; repeat the read via subprocess.
    Fallback,
}
/// Result of a daemon call.
type Outcome<T> = Result<T, Failure>;

/// Largest upload sent through the daemon. The body is buffered (the daemon
/// gets its size up front and uploads it with a known size), so this bounds
/// memory per upload; larger shards are dominated by transfer time, not by
/// the per-request cost the daemon saves, and keep the subprocess path.
pub(super) const DAEMON_UPLOAD_MAX: u64 = 16 * 1024 * 1024;

/// Backends with an `upload_concurrency` option (chunks of one file sent in
/// parallel), from `rclone help flags`; others ignore the variable.
const CHUNKED_UPLOAD_BACKENDS: [&str; 12] = [
    "azureblob",
    "azurefiles",
    "b2",
    "drime",
    "filen",
    "hidrive",
    "internxt",
    "oos",
    "pikpak",
    "qingstor",
    "s3",
    "shade",
];

/// Registry key: one daemon per distinct executable, config and environment.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    /// Reads or uploads (separate daemons, see [`Role`]).
    role: Role,
    /// rclone binary.
    executable: PathBuf,
    /// Explicit `--config` file; `None` = inherited selection.
    config: Option<PathBuf>,
    /// Full (sorted) environment, since it can change rclone behavior.
    environment: Vec<(OsString, OsString)>,
}

/// What a daemon serves. Uploads get their own daemon, started with rclone
/// retries off (`--low-level-retries 1 --retries 1`, like the `rcat`
/// subprocess): a backend's pacer takes its retry count when the backend is
/// first built, so a per-call setting cannot turn retries off on a backend
/// the read daemon already built. Reads keep rclone's default retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Role {
    /// Stats, listings and ranged reads.
    Read,
    /// Small uploads (`Daemon::upload`).
    Upload,
}

/// Config file (size, mtime) at daemon start; `None` = unreadable. A change means the daemon is stale.
pub(super) type Fingerprint = Option<(u64, Option<SystemTime>)>;

/// Registry entry for one `Key`.
#[derive(Default)]
struct Slot {
    /// Current daemon, if one is running.
    daemon: Option<Arc<Daemon>>,
    /// Restarts after a crash so far (only one is allowed).
    restarts: u32,
    /// True once this key permanently falls back to subprocesses.
    disabled: bool,
    /// Resolved once: `Some(path)` of the effective rclone config file.
    config_file: Option<PathBuf>,
}

/// Process-wide map of daemon slots.
fn registry() -> &'static Mutex<HashMap<Key, Arc<Mutex<Slot>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Key, Arc<Mutex<Slot>>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
/// Set by `shutdown`; afterwards no new daemon is started.
static SHUT_DOWN: AtomicBool = AtomicBool::new(false);

/// One running `rclone rcd` and how to reach it.
pub(super) struct Daemon {
    /// The rcd process (or a wrapper script around it).
    child: Mutex<Child>,
    /// Unix socket or loopback TCP address with credentials.
    endpoint: Endpoint,
    /// Set once the process is known to have exited or was terminated.
    dead: AtomicBool,
    /// Config fingerprint this daemon was started with.
    fingerprint: Fingerprint,
    /// Rate last set with `core/bwlimit` (`None` = rclone's default, off).
    bwlimit: Mutex<Option<String>>,
    /// Serves uploads ([`Role::Upload`]): RPool already paces the bytes it
    /// sends, so no rclone bandwidth limit is set on it.
    uploads: bool,
    /// 0700 temp dir holding the socket and owner file; removed on drop.
    #[cfg(unix)]
    dir: tempfile::TempDir,
}
impl Daemon {
    /// Marks the daemon dead, asks it to quit via `core/quit`, then kills the
    /// child (and on unix its children) and reaps it.
    fn terminate(&self) {
        self.dead.store(true, Ordering::Release);
        // Ask rclone itself to quit first: when the configured executable is
        // a wrapper script, killing the child pid would orphan the real rcd.
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(2));
        let _ = http::send(
            &self.endpoint,
            &ctx,
            "POST",
            "/core/quit",
            &[("Content-Type", "application/json")],
            Some(b"{}"),
        );
        if let Ok(mut child) = self.child.lock() {
            #[cfg(unix)]
            let _ = Command::new("pkill")
                .args(["-TERM", "-P", &child.id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    /// Whether the process has exited (cached once true).
    fn is_dead(&self) -> bool {
        if self.dead.load(Ordering::Acquire) {
            return true;
        }
        let exited = self
            .child
            .lock()
            .map_or(true, |mut child| !matches!(child.try_wait(), Ok(None)));
        if exited {
            self.dead.store(true, Ordering::Release);
        }
        exited
    }
    /// Sets the daemon's bandwidth (`core/bwlimit rate=`) unless it already
    /// has `rate`. A failed call is retried on the next scheduler step.
    pub(super) fn apply_bwlimit(&self, rate: &str) {
        let mut applied = self.bwlimit.lock().unwrap_or_else(|p| p.into_inner());
        let current = applied.as_deref().unwrap_or("off");
        if current == rate || self.uploads || self.is_dead() {
            return;
        }
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(5));
        if self
            .call(
                &ctx,
                "core/bwlimit",
                &json!({ "rate": rate }),
                ERROR_BODY_LIMIT,
            )
            .is_ok()
        {
            *applied = Some(rate.to_owned());
        }
    }
    #[cfg(all(test, unix))]
    pub(super) fn bwlimit_rate(&self) -> Option<String> {
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(5));
        let reply = self
            .call(&ctx, "core/bwlimit", &json!({}), ERROR_BODY_LIMIT)
            .ok()?;
        reply.get("rate").and_then(Value::as_str).map(str::to_owned)
    }
    #[cfg(all(test, unix))]
    pub(super) fn pid(&self) -> u32 {
        self.child.lock().unwrap().id()
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.terminate(); // The temp dir (socket, owner file) is removed after.
    }
}

/// Kills every daemon of this process when dropped. Held by `main`.
pub(crate) struct ShutdownGuard;
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        shutdown();
    }
}
/// Whether this process is shutting its daemons down.
pub(super) fn shut_down() -> bool {
    SHUT_DOWN.load(Ordering::Acquire)
}

/// Every running daemon of this process.
pub(super) fn live() -> Vec<Arc<Daemon>> {
    let slots: Vec<_> = registry()
        .lock()
        .map(|table| table.values().cloned().collect())
        .unwrap_or_default();
    slots
        .iter()
        .filter_map(|slot| slot.lock().ok()?.daemon.clone())
        .filter(|daemon| !daemon.is_dead())
        .collect()
}

/// Terminates every daemon and blocks new ones; run by `ShutdownGuard` on exit.
pub(crate) fn shutdown() {
    SHUT_DOWN.store(true, Ordering::Release);
    let slots: Vec<_> = registry()
        .lock()
        .map(|mut map| map.drain().map(|(_, slot)| slot).collect())
        .unwrap_or_default();
    for slot in slots {
        if let Ok(mut slot) = slot.lock() {
            if let Some(daemon) = slot.daemon.take() {
                daemon.terminate();
            }
        }
    }
}

/// True when `RPOOL_RCLONE_DAEMON` is set to `0`/`false`/`no`/`off`.
fn opted_out(context: &RcloneContext) -> bool {
    context.environment.iter().any(|(key, value)| {
        key == "RPOOL_RCLONE_DAEMON"
            && matches!(
                value.to_string_lossy().trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
    })
}

/// Non-empty value of environment variable `name` in the context's environment.
fn env_value<'a>(context: &'a RcloneContext, name: &str) -> Option<&'a OsString> {
    context
        .environment
        .iter()
        .find(|(key, value)| key == name && !value.is_empty())
        .map(|(_, value)| value)
}

/// Effective config file, exactly as the subprocess path would use it.
pub(super) fn config_file(context: &RcloneContext) -> Option<PathBuf> {
    if let ConfigSelection::File(path) = &context.config {
        return Some(path.clone());
    }
    if let Some(path) = env_value(context, "RCLONE_CONFIG") {
        return Some(path.into());
    }
    let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(10));
    let bytes = context.capture(&ctx, &["config", "file"]).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let path = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    PathBuf::from(path)
        .is_absolute()
        .then(|| PathBuf::from(path))
}

/// (size, mtime) of `path`, or `None` if it cannot be read.
pub(super) fn fingerprint(path: &std::path::Path) -> Fingerprint {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()))
}

/// The live daemon for this context, starting or replacing it when needed.
/// `None` = use subprocesses.
pub(super) fn get(context: &RcloneContext) -> Option<Arc<Daemon>> {
    get_role(context, Role::Read)
}

/// The live upload daemon for this context ([`Role::Upload`]).
pub(super) fn get_upload(context: &RcloneContext) -> Option<Arc<Daemon>> {
    get_role(context, Role::Upload)
}

/// The live daemon of `role` for this context, starting or replacing it
/// when needed. `None` = use subprocesses.
fn get_role(context: &RcloneContext, role: Role) -> Option<Arc<Daemon>> {
    if !context.daemon_allowed || opted_out(context) || SHUT_DOWN.load(Ordering::Acquire) {
        return None;
    }
    let mut environment = context.environment.clone();
    environment.sort();
    let key = Key {
        role,
        executable: context.executable.clone(),
        config: match &context.config {
            ConfigSelection::File(path) => Some(path.clone()),
            ConfigSelection::Inherited => None,
        },
        environment,
    };
    let slot = registry().lock().ok()?.entry(key).or_default().clone();
    let mut slot = slot.lock().ok()?;
    if slot.disabled {
        return None;
    }
    if slot.config_file.is_none() {
        match config_file(context) {
            Some(path) => slot.config_file = Some(path),
            None => {
                // Without the config path, staleness cannot be detected.
                slot.disabled = true;
                return None;
            }
        }
    }
    let current = fingerprint(slot.config_file.as_deref()?);
    if let Some(daemon) = slot.daemon.clone() {
        if daemon.is_dead() {
            slot.daemon = None;
            if slot.restarts >= 1 {
                slot.disabled = true;
                return None;
            }
            slot.restarts += 1;
        } else if daemon.fingerprint == current {
            return Some(daemon);
        } else {
            // Config changed: in-flight reads finish on the old daemon, which
            // is killed when its last user drops it.
            slot.daemon = None;
        }
    }
    match start(context, current, role) {
        Some(daemon) => {
            let daemon = Arc::new(daemon);
            slot.daemon = Some(daemon.clone());
            super::bwlimit_schedule::on_start(&daemon);
            Some(daemon)
        }
        None => {
            slot.disabled = true;
            None
        }
    }
}

/// `rcd` command with the context's global args/env, minus `RCLONE_RC_*`
/// variables, with stdin/stdout discarded.
fn daemon_command(context: &RcloneContext, args: &[&str], role: Role) -> Command {
    let mut args: Vec<OsString> = args.iter().map(OsString::from).collect();
    if role == Role::Upload {
        // Like `rcat --retries 1 --low-level-retries 1`: a refused write
        // reaches RPool (rate limits, Dropbox's concurrent-write refusal)
        // instead of being retried inside rclone while a permit is held.
        args.extend(["--low-level-retries", "1", "--retries", "1"].map(OsString::from));
    }
    let mut command = context.base_command(&args);
    if role == Role::Upload {
        // One shard is one upload request here too: no chunk fan-out inside
        // a shard (backend-wide variables override rclone.conf; see
        // `write::chunk_concurrency_variable` for the subprocess path).
        for backend in CHUNKED_UPLOAD_BACKENDS {
            command.env(
                format!("RCLONE_{}_UPLOAD_CONCURRENCY", backend.to_ascii_uppercase()),
                "1",
            );
        }
    }
    for (key, _) in &context.environment {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_RC_")
        {
            command.env_remove(key);
        }
    }
    command.stdin(Stdio::null()).stdout(Stdio::null());
    command
}

/// Starts `rclone rcd` on a unix socket in a fresh 0700 temp dir (sweeping
/// stale daemons first) and waits until it is ready; `None` on any failure.
#[cfg(unix)]
fn start(context: &RcloneContext, fingerprint: Fingerprint, role: Role) -> Option<Daemon> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("rpool-rcd-")
        .tempdir()
        .ok()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).ok()?;
    sweep_stale(dir.path());
    let socket = dir.path().join("rc.sock");
    // sun_path is 104 (macOS) / 108 (Linux) bytes including the NUL.
    if socket.as_os_str().len() > 100 {
        return None;
    }
    let address = format!("unix://{}", socket.to_str()?);
    let mut command = daemon_command(
        context,
        &["rcd", "--rc-serve", "--rc-no-auth", "--rc-addr", &address],
        role,
    );
    command.stderr(Stdio::null());
    let child = command.spawn().ok()?;
    let owner = format!("{}\n{}\n", std::process::id(), child.id());
    let daemon = Daemon {
        child: Mutex::new(child),
        endpoint: Endpoint::Unix(socket),
        dead: AtomicBool::new(false),
        fingerprint,
        bwlimit: Mutex::new(None),
        uploads: role == Role::Upload,
        dir,
    };
    std::fs::write(daemon.dir.path().join("owner"), owner).ok()?;
    ready(&daemon).then_some(daemon)
}

/// Starts `rclone rcd` on 127.0.0.1 with a random port and Basic-auth password
/// passed via env, reads the port from stderr and waits until it is ready.
#[cfg(windows)]
fn start(context: &RcloneContext, fingerprint: Fingerprint, role: Role) -> Option<Daemon> {
    use std::io::{BufRead, Read};
    use std::os::windows::process::CommandExt;
    let mut secret = [0u8; 24];
    getrandom::fill(&mut secret).ok()?;
    let password = data_encoding::HEXLOWER.encode(&secret);
    let mut command = daemon_command(
        context,
        &["rcd", "--rc-serve", "--rc-addr", "127.0.0.1:0"],
        role,
    );
    command
        .env("RCLONE_RC_USER", "rpool")
        .env("RCLONE_RC_PASS", &password)
        .stderr(Stdio::piped())
        .creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command.spawn().ok()?;
    let stderr = child.stderr.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    // Drains stderr for the daemon's lifetime; only the port is kept.
    std::thread::spawn(move || {
        let mut reader = io::BufReader::new(stderr);
        let mut line = Vec::new();
        let marker = "Serving remote control on http://127.0.0.1:";
        let mut sent = false;
        loop {
            line.clear();
            match (&mut reader).take(16 * 1024).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if sent {
                continue;
            }
            let text = String::from_utf8_lossy(&line);
            if let Some(rest) = text.split(marker).nth(1) {
                let port: String = rest.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(port) = port.parse::<u16>() {
                    sent = true;
                    let _ = sender.send(port);
                }
            }
        }
    });
    let daemon = |endpoint| Daemon {
        child: Mutex::new(child),
        endpoint,
        dead: AtomicBool::new(false),
        fingerprint,
        bwlimit: Mutex::new(None),
        uploads: role == Role::Upload,
    };
    let Ok(port) = receiver.recv_timeout(READY_TIMEOUT) else {
        drop(daemon(Endpoint::Tcp {
            address: ([127, 0, 0, 1], 0).into(),
            authorization: String::new(),
        }));
        return None;
    };
    let credentials = data_encoding::BASE64.encode(format!("rpool:{password}").as_bytes());
    let daemon = daemon(Endpoint::Tcp {
        address: ([127, 0, 0, 1], port).into(),
        authorization: format!("Basic {credentials}"),
    });
    ready(&daemon).then_some(daemon)
}

/// Polls `rc/noop` until it answers 200 within `READY_TIMEOUT`; false if the daemon dies or times out.
fn ready(daemon: &Daemon) -> bool {
    let until = Instant::now() + READY_TIMEOUT;
    while Instant::now() < until {
        if daemon.is_dead() {
            return false;
        }
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(1));
        if let Ok(mut response) = http::send(
            &daemon.endpoint,
            &ctx,
            "POST",
            "/rc/noop",
            &[("Content-Type", "application/json")],
            Some(b"{}"),
        ) {
            if response.status == 200 && response.body(&ctx, ERROR_BODY_LIMIT).is_ok() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Removes temp dirs of daemons whose RPool owner is gone, killing the daemon
/// only when its live command line still names that dir's socket.
#[cfg(unix)]
fn sweep_stale(own: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let (Some(parent), Ok(own_meta)) = (own.parent(), std::fs::metadata(own)) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_candidate = path != own
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("rpool-rcd-"))
                && std::fs::symlink_metadata(&path)
                    .is_ok_and(|meta| meta.is_dir() && meta.uid() == own_meta.uid());
            if !is_candidate {
                continue;
            }
            let Ok(owner) = std::fs::read_to_string(path.join("owner")) else {
                continue; // Starting up, or not ours to judge.
            };
            let mut pids = owner.lines().map(|line| line.trim().parse::<u32>());
            let (Some(Ok(rpool)), Some(Ok(rcd))) = (pids.next(), pids.next()) else {
                continue;
            };
            if !matches!(ps_command(rpool), Some(None)) {
                continue; // Owner alive, or unknown.
            }
            let socket = path.join("rc.sock");
            match ps_command(rcd) {
                Some(Some(line)) if socket.to_str().is_some_and(|s| line.contains(s)) => {
                    let _ = Command::new("kill")
                        .args(["-9", &rcd.to_string()])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                Some(_) => {}
                None => continue,
            }
            let _ = std::fs::remove_dir_all(&path);
        }
    });
}

/// `Some(Some(command line))` running, `Some(None)` not running, `None` unknown.
#[cfg(unix)]
fn ps_command(pid: u32) -> Option<Option<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match (output.status.success(), text.is_empty()) {
        (true, false) => Some(Some(text)),
        (false, true) => Some(None),
        _ => None,
    }
}

/// `(fs root, path below it)` for addresses the daemon can serve with the
/// same meaning as the CLI; `None` sends the call to the subprocess path.
/// One root per remote keeps a single warm backend instance.
fn split(address: &str) -> Option<(String, String)> {
    let name = remote_name(address).ok()?;
    if name.contains(['[', ']']) {
        return None;
    }
    let path = &address[name.len() + 1..];
    if path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return None;
    }
    let (root, rest) = match path.strip_prefix('/') {
        Some(rest) => (format!("{name}:/"), rest),
        None => (format!("{name}:"), path),
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if !rest.is_empty() && rest.split('/').any(|s| matches!(s, "" | "." | "..")) {
        return None;
    }
    Some((root, rest.to_owned()))
}

/// Classifies an rc error reply: denials, 404 and rate limits are definite;
/// unparseable or other errors fall back to subprocess.
fn error_failure(status: u16, body: &[u8]) -> Failure {
    let Some(message) = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    else {
        return Failure::Fallback;
    };
    if let Some(error) = process::denial(&message) {
        Failure::Definite(error)
    } else if status == 404 {
        Failure::Definite(StorageError::not_found("rclone object"))
    } else if let Some(error) = process::rate_limited(&message) {
        Failure::Definite(error)
    } else {
        Failure::Fallback
    }
}

impl Daemon {
    /// Maps an HTTP failure: stop/sink errors are definite, transport errors fall
    /// back (and mark a crashed daemon dead).
    fn transport(&self, error: HttpError) -> Failure {
        match error {
            HttpError::Stopped(error) => Failure::Definite(error),
            HttpError::Sink(error) => Failure::Definite(process::sink_error(error)),
            HttpError::Transport => {
                let _ = self.is_dead(); // Marks a crashed daemon for restart.
                Failure::Fallback
            }
        }
    }

    /// POSTs `input` to rc `method` and parses the JSON reply, reading at most
    /// `limit` bytes of a 200 body.
    fn call(
        &self,
        ctx: &OperationContext,
        method: &str,
        input: &Value,
        limit: usize,
    ) -> Outcome<Value> {
        let body = serde_json::to_vec(input).map_err(|_| Failure::Fallback)?;
        let mut response = http::send(
            &self.endpoint,
            ctx,
            "POST",
            &format!("/{method}"),
            &[("Content-Type", "application/json")],
            Some(&body),
        )
        .map_err(|error| self.transport(error))?;
        let mut sink = BoundedVec {
            bytes: Vec::new(),
            limit: if response.status == 200 {
                limit
            } else {
                ERROR_BODY_LIMIT
            },
        };
        match response.copy_to(ctx, &mut sink) {
            Ok(_) => {}
            Err(HttpError::Sink(error)) if response.status == 200 => {
                return Err(Failure::Definite(process::sink_error(error)))
            }
            Err(HttpError::Sink(_)) => return Err(Failure::Fallback),
            Err(error) => return Err(self.transport(error)),
        }
        if response.status != 200 {
            return Err(error_failure(response.status, &sink.bytes));
        }
        serde_json::from_slice(&sink.bytes).map_err(|_| Failure::Fallback)
    }

    /// The `lsjson --stat` item of `address` (`opt` as for lsjson).
    pub(super) fn stat(&self, ctx: &OperationContext, address: &str, opt: Value) -> Outcome<Value> {
        let (root, rel) = split(address).ok_or(Failure::Fallback)?;
        let reply = self.call(
            ctx,
            "operations/stat",
            &json!({"fs": root, "remote": rel, "opt": opt}),
            ADMIN_LIMIT,
        )?;
        match reply.get("item") {
            Some(item) if item.is_object() => return Ok(item.clone()),
            Some(Value::Null) if !rel.is_empty() => {}
            _ => return Err(Failure::Fallback),
        }
        // Not an object or listed directory. `lsjson --stat` roots the fs at
        // the full path, so e.g. bucket backends report a missing key as a
        // directory: ask exactly that way.
        let reply = self.call(
            ctx,
            "operations/stat",
            &json!({"fs": address, "remote": "", "opt": opt}),
            ADMIN_LIMIT,
        )?;
        match reply.get("item") {
            Some(item) if item.is_object() => Ok(item.clone()),
            Some(Value::Null) => Err(Failure::Definite(StorageError::not_found("rclone object"))),
            _ => Err(Failure::Fallback),
        }
    }

    /// `lsjson -R --no-mimetype` bytes of the tree at `address`.
    pub(super) fn list(
        &self,
        ctx: &OperationContext,
        address: &str,
        limit: usize,
    ) -> Outcome<Vec<u8>> {
        let (root, rel) = split(address).ok_or(Failure::Fallback)?;
        let reply = match self.call(
            ctx,
            "operations/list",
            &json!({"fs": root, "remote": rel, "opt": {"recurse": true, "noMimeType": true}}),
            limit,
        ) {
            Ok(reply) => reply,
            Err(Failure::Definite(StorageError::NotFound { .. })) => {
                // `lsjson` of a file lists that file; only a confirmed
                // missing path is NotFound.
                return match self.stat(ctx, address, json!({})) {
                    Ok(_) => Err(Failure::Fallback),
                    Err(failure) => Err(failure),
                };
            }
            Err(failure) => return Err(failure),
        };
        let Some(Value::Array(entries)) = reply.get("list") else {
            return Err(Failure::Fallback);
        };
        let prefix = format!("{rel}/");
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut entry = entry.clone();
            if !rel.is_empty() {
                let path = entry.get("Path").and_then(Value::as_str);
                let Some(stripped) = path.and_then(|p| p.strip_prefix(&prefix)) else {
                    return Err(Failure::Fallback);
                };
                entry["Path"] = Value::String(stripped.to_owned());
            }
            out.push(entry);
        }
        let bytes = serde_json::to_vec(&Value::Array(out)).map_err(|_| Failure::Fallback)?;
        if bytes.len() > limit {
            return Err(Failure::Definite(StorageError::OutputBoundsViolated));
        }
        Ok(bytes)
    }

    /// Streams `count` bytes (all when `None`) from `offset` into `sink`,
    /// adding to `written` what reached the sink even when failing, so a
    /// fallback can resume instead of duplicating bytes.
    pub(super) fn read(
        &self,
        ctx: &OperationContext,
        address: &str,
        offset: u64,
        count: Option<u64>,
        sink: &mut dyn Write,
        written: &mut u64,
    ) -> Outcome<()> {
        let (root, rel) = split(address).ok_or(Failure::Fallback)?;
        if rel.is_empty() || address.ends_with('/') {
            return Err(Failure::Fallback); // A directory listing page, never a file.
        }
        let mut target = String::from("/%5B");
        http::encode_component(&root, &mut target);
        target.push_str("%5D");
        for segment in rel.split('/') {
            target.push('/');
            http::encode_component(segment, &mut target);
        }
        let range = match count {
            Some(count) => Some(format!("bytes={offset}-{}", offset + count - 1)),
            None if offset > 0 => Some(format!("bytes={offset}-")),
            None => None,
        };
        let headers: Vec<(&str, &str)> = range.iter().map(|r| ("Range", r.as_str())).collect();
        let mut response = http::send(&self.endpoint, ctx, "GET", &target, &headers, None)
            .map_err(|error| self.transport(error))?;
        let expected = if range.is_some() { 206 } else { 200 };
        if response.status != expected {
            let body = response
                .body(ctx, ERROR_BODY_LIMIT)
                .map_err(|error| self.transport(error))?;
            // A 404 for a range past the end is plain text; only rclone's
            // JSON error means a missing object.
            return Err(if response.header("content-range").is_some() {
                Failure::Fallback
            } else {
                error_failure(response.status, &body)
            });
        }
        if range.is_some() {
            let starts_at = response
                .header("content-range")
                .and_then(|value| value.strip_prefix("bytes "))
                .and_then(|value| value.split('-').next())
                .and_then(|start| start.trim().parse::<u64>().ok());
            if starts_at != Some(offset) {
                return Err(Failure::Fallback);
            }
        }
        let mut counting = Counting { sink, written };
        response
            .copy_to(ctx, &mut counting)
            .map(drop)
            .map_err(|error| self.transport(error))
    }
}

impl Daemon {
    /// Uploads `body` as the object at `address` (`operations/uploadfile`)
    /// with a known size, on the upload daemon (rclone retries off), like
    /// `rcat --size --low-level-retries 1`. Errors are classified like the
    /// subprocess's (`process::classify_mutation`): a refusal is retriable,
    /// anything else an unknown outcome. Once the request is sent, a stop or
    /// deadline is an unknown outcome (the write may complete), and no answer
    /// within the stall allowance is the retriable stall timeout (a re-send
    /// overwrites the same object). Only a daemon that cannot take the
    /// request (transport, an answer that is not rclone's) falls back.
    pub(super) fn upload(&self, ctx: &OperationContext, address: &str, body: &[u8]) -> Outcome<()> {
        let (root, rel) = split(address).ok_or(Failure::Fallback)?;
        let (dir, name) = match rel.rsplit_once('/') {
            Some((dir, name)) => (dir, name),
            None => ("", rel.as_str()),
        };
        if name.is_empty() || address.ends_with('/') {
            return Err(Failure::Fallback);
        }
        process::check(ctx).map_err(Failure::Definite)?;
        let config = json!({
            // Buffer exactly this body (a bare number is KiB): uploaded with
            // its size, never streamed (a backend without streaming uploads
            // would spool to disk).
            "StreamingUploadCutoff": format!("{}", body.len() as u64 / 1024 + 1),
            "LowLevelRetries": 1,
        })
        .to_string();
        let mut target = String::from("/operations/uploadfile?fs=");
        http::encode_component(&root, &mut target);
        target.push_str("&remote=");
        http::encode_component(dir, &mut target);
        target.push_str("&_config=");
        http::encode_component(&config, &mut target);
        let (content_type, head, tail) = multipart(name);
        let allowance = super::stall::STALL_FLOOR.max(Duration::from_secs(
            body.len() as u64 / super::stall::STALL_RATE,
        ));
        let bounded = ctx.with_earlier_deadline(Instant::now() + allowance);
        let stopped = |error: HttpError| match error {
            HttpError::Stopped(_) if ctx.is_cancelled() || ctx.deadline_passed() => {
                Failure::Definite(StorageError::unknown_outcome(
                    "daemon upload stopped before its answer",
                ))
            }
            HttpError::Stopped(_) => Failure::Definite(StorageError::Timeout {
                detail: process::STALLED_DETAIL.into(),
            }),
            other => self.transport(other),
        };
        let mut response = http::send_parts(
            &self.endpoint,
            &bounded,
            "POST",
            &target,
            &[("Content-Type", content_type.as_str())],
            Some(&[head.as_slice(), body, tail.as_slice()]),
        )
        .map_err(stopped)?;
        let reply = response.body(&bounded, ERROR_BODY_LIMIT).map_err(stopped)?;
        if response.status == 200 {
            return Ok(());
        }
        Err(upload_failure(&reply))
    }
}

/// The framing of one `multipart/form-data` part named `file`: (Content-Type,
/// bytes before the body, bytes after it). The file name travels as RFC 2231
/// `filename*` (UTF-8, percent-encoded), so encrypted names with any
/// characters arrive unchanged.
fn multipart(name: &str) -> (String, Vec<u8>, Vec<u8>) {
    let mut random = [0u8; 12];
    let _ = getrandom::fill(&mut random);
    let boundary: String = std::iter::once("rpool".to_owned())
        .chain(random.iter().map(|b| format!("{b:02x}")))
        .collect();
    let mut encoded = String::new();
    http::encode_component(name, &mut encoded);
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename*=UTF-8''{encoded}\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    let tail = format!("\r\n--{boundary}--\r\n").into_bytes();
    (
        format!("multipart/form-data; boundary={boundary}"),
        head,
        tail,
    )
}

/// Classifies a failed upload reply like a failed `rcat`
/// (`process::classify_mutation`). Only an answer that is not rclone's JSON
/// error (the daemon itself misbehaving) falls back to the subprocess.
fn upload_failure(body: &[u8]) -> Failure {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    match message {
        Some(message) => Failure::Definite(process::classify_mutation(message.as_bytes())),
        None => Failure::Fallback,
    }
}

/// Write adapter that forwards bytes to `sink` and counts them in `written`.
struct Counting<'a, 'b> {
    /// Caller's destination.
    sink: &'a mut dyn Write,
    /// Running byte count, updated after each successful write.
    written: &'b mut u64,
}
impl Write for Counting<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.sink.write_all(bytes)?;
        *self.written += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_one_root_per_remote_and_refuses_ambiguous_paths() {
        assert_eq!(split("c:a/b c/d").unwrap(), ("c:".into(), "a/b c/d".into()));
        assert_eq!(split("c:").unwrap(), ("c:".into(), String::new()));
        assert_eq!(split("c:dir/").unwrap(), ("c:".into(), "dir".into()));
        assert_eq!(
            split("base:/private/tmp/x").unwrap(),
            ("base:/".into(), "private/tmp/x".into())
        );
        for refused in [
            "c:a//b",
            "c:a/../b",
            "c:./a",
            "c:a\\b",
            "c:a:b",
            "c]:x",
            "C:/windows",
            "nocolon",
            "c:a\nb",
        ] {
            assert!(split(refused).is_none(), "{refused}");
        }
    }

    #[test]
    fn upload_errors_are_classified_like_rcat() {
        let reply = |message: &str| serde_json::to_vec(&json!({ "error": message })).unwrap();
        let kind = |failure: Failure| match failure {
            Failure::Definite(error) => Some(error),
            Failure::Fallback => None,
        };
        // A refused write is retriable, as from rcat.
        let refused = kind(upload_failure(&reply("HTTP error 429: too many requests"))).unwrap();
        assert!(
            matches!(refused, StorageError::RateLimited { .. }),
            "{refused:?}"
        );
        let dropbox = kind(upload_failure(&reply("too_many_write_operations/"))).unwrap();
        assert!(matches!(dropbox, StorageError::RateLimited { .. }));
        // A path that merely contains 429 or a denial word is no signal: the
        // write may have happened.
        for message in [
            "failed to upload x/shard-429/a: connection reset",
            "access denied while finishing x",
            "insufficient space",
        ] {
            let error = kind(upload_failure(&reply(message))).unwrap();
            assert_eq!(
                error.kind(),
                crate::storage::error::StorageErrorKind::UnknownOutcome,
                "{message}"
            );
        }
        // Only an answer that is not rclone's falls back to the subprocess.
        assert!(kind(upload_failure(b"<html>bad gateway</html>")).is_none());
    }

    #[test]
    fn multipart_framing_carries_any_name() {
        let (content_type, head, tail) = multipart("ꕋ a+b%20/x.bin");
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with(&format!("--{boundary}\r\n")));
        assert!(
            head.contains("filename*=UTF-8''%EA%95%8B%20a%2Bb%2520%2Fx.bin"),
            "{head}"
        );
        assert_eq!(tail, format!("\r\n--{boundary}--\r\n").into_bytes());
    }

    #[test]
    fn error_bodies_map_to_definite_answers_or_fallback() {
        let kind = |status, body: &str| match error_failure(status, body.as_bytes()) {
            Failure::Definite(error) => Some(error.kind()),
            Failure::Fallback => None,
        };
        use crate::storage::error::StorageErrorKind as K;
        assert_eq!(
            kind(
                404,
                r#"{"error":"failed to find object: object not found","status":404}"#
            ),
            Some(K::NotFound)
        );
        assert_eq!(
            kind(
                500,
                r#"{"error":"couldn't list: 401 Unauthorized","status":500}"#
            ),
            Some(K::Authentication)
        );
        assert_eq!(
            kind(500, r#"{"error":"permission denied","status":500}"#),
            Some(K::PermissionDenied)
        );
        assert_eq!(
            kind(500, r#"{"error":"HTTP 429: rate limit","status":500}"#),
            Some(K::RateLimited)
        );
        assert_eq!(kind(500, r#"{"error":"is a directory not a file"}"#), None);
        assert_eq!(kind(404, "Not Found"), None);
        assert_eq!(
            kind(500, r#"{"error":"didn't find section in config file"}"#),
            None
        );
    }
}
