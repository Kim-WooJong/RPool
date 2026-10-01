//! Owned rclone mount process. Local/VFS data is deliberately never deleted here.
use anyhow::{bail, Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound for moving rclone's delayed write-back queue into the WebDAV
/// backend before quitting. Remaining items stay in the durable VFS cache.
const WRITEBACK_DRAIN_LIMIT: Duration = Duration::from_secs(90);
const LOG_LINE_LIMIT: usize = 4096;
const LOG_READ_LIMIT: u64 = 1024 * 1024;
const LOG_LINES_RETURNED: usize = 200;

pub(crate) struct MountConfig {
    pub(crate) rclone: String,
    pub(crate) files_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) shared: bool,
    pub(crate) read_only: bool,
    pub(crate) vfs_cache_gib: u64,
    pub(crate) cache_min_free_gib: u64,
    pub(crate) webdav: Option<(String, String)>,
    /// OS volume label (Explorer/Finder name); `None` keeps rclone's default.
    pub(crate) volume_name: Option<String>,
}

pub(crate) struct StopReport {
    pub(crate) forced: bool,
    pub(crate) cache_preserved: bool,
}

pub(crate) struct MountProcess {
    child: Child,
    target: PathBuf,
    address: SocketAddr,
    credential: String,
    logs: Mutex<MountLog>,
    stopped: bool,
    graceful_quit_requested: bool,
    shutdown_uncertain: bool,
    lease: MountLease,
}

impl MountProcess {
    pub(crate) fn start(config: MountConfig) -> Result<Self> {
        validate_mountpoint(&config)?;
        #[cfg(target_os = "macos")]
        check_nfsmount_version(&config.rclone)?;
        #[cfg(target_os = "macos")]
        let target = config.target.canonicalize()?;
        #[cfg(not(target_os = "macos"))]
        let target = config.target.clone();
        std::fs::create_dir_all(&config.cache_dir).context("cannot create durable VFS cache")?;
        let files = config.files_dir.canonicalize()?;
        let cache = config.cache_dir.canonicalize()?;
        let lease = MountLease::prepare(&files, &cache, &target, config.webdav.as_ref())?;
        let log_path = files
            .parent()
            .context("workspace root missing")?
            .join(".rpool")
            .join("rclone-mount.log");
        let log = open_mount_log(&log_path)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|e| anyhow::anyhow!("cannot generate mount credentials: {e}"))?;
        let password = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let credential = base64(format!("rpool:{password}").as_bytes());
        let mut source = std::ffi::OsString::from(":local:");
        source.push(files.as_os_str());
        if config.webdav.is_some() {
            source = ":webdav:".into();
        }
        let (cache_mode, write_back) = vfs_cache_policy(config.webdav.is_some());
        let mut command = Command::new(&config.rclone);
        command
            .arg(native_mount_command())
            .arg(source)
            .arg(&target)
            .args([
                "--vfs-cache-mode",
                cache_mode,
                "--vfs-write-back",
                write_back,
                "--cache-dir",
            ])
            .arg(cache)
            .args(["--rc", "--rc-addr", &address.to_string()])
            .env("RCLONE_RC_USER", "rpool")
            .env("RCLONE_RC_PASS", &password)
            .env("RCLONE_RC_NO_AUTH", "false")
            // rclone writes its own log file, never a pipe: if RPool exits first,
            // a pipe would kill rclone (SIGPIPE) while the kernel mount still needs it.
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        if let Some((url, token)) = &config.webdav {
            command
                .env("RCLONE_WEBDAV_URL", url)
                .env("RCLONE_WEBDAV_BEARER_TOKEN", token)
                .env("RCLONE_WEBDAV_VENDOR", "other")
                .env("RCLONE_WEBDAV_USER", "")
                .env("RCLONE_WEBDAV_PASS", "")
                .env("RCLONE_WEBDAV_BEARER_TOKEN_COMMAND", "")
                .args(["--dir-cache-time", "2s", "--vfs-read-chunk-size", "0"]);
        }
        if config.read_only {
            command.arg("--read-only");
        }
        if let Some(name) = config.volume_name.as_deref().map(volume_label) {
            command.arg("--volname").arg(name);
        }
        configure_cache(
            &mut command,
            config.vfs_cache_gib,
            config.cache_min_free_gib,
        );
        if config.shared {
            // Only rclone may evict its clean, unused cache entries. Short
            // retention allows an unmounted shared reconciliation without
            // deleting potentially dirty cache files ourselves.
            command.args(["--vfs-cache-max-age", "1s"]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        // rclone must bind this port itself. Authentication protects the brief bind race;
        // a stolen port produces an error, not an unauthenticated control connection.
        drop(listener);
        validate_mountpoint(&config)?;
        // Persist uncertainty BEFORE spawning: a crash in the spawn/record gap must fail closed.
        lease.record(&LeaseState::Launching)?;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                // spawn returned an error, so no owned child was launched.
                lease.clear()?;
                return Err(error).context(
                    "cannot start rclone native mount; check rclone and the platform mount support",
                );
            }
        };
        if let Err(error) = lease.record(&LeaseState::Running { pid: child.id() }) {
            // Do not discard a launching lease unless termination has actually been observed.
            if child.kill().is_ok() && child.wait().is_ok() {
                let _ = lease.clear_if_unmounted(&target);
            }
            return Err(error).context("cannot record mount child; uncertain lease retained unless child termination was confirmed");
        }
        let mut secrets = vec![password, credential.clone()];
        secrets.extend(config.webdav.as_ref().map(|(_, token)| token.clone()));
        let logs = Mutex::new(MountLog::new(log_path, secrets));
        Ok(Self {
            child,
            target,
            address,
            credential,
            logs,
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: false,
            lease,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if let Some(_exit) = &status {
            self.stopped = true;
            #[cfg(target_os = "macos")]
            if !self.graceful_quit_requested || !_exit.success() {
                self.lease.record(&LeaseState::ShutdownUncertain {
                    pid: self.child.id(),
                })?;
                self.shutdown_uncertain = true;
                bail!("macOS NFS process exited without a confirmed graceful unmount ({_exit}); mount lease retained for OS I/O inspection");
            }
            self.lease.clear_if_unmounted(&self.target)?;
        }
        Ok(status)
    }

    /// Accessibility alone is insufficient on Unix because the mount directory preexists.
    pub(crate) fn ready(&self) -> bool {
        if self.stopped || self.rc("vfs/stats").is_err() {
            return false;
        }
        #[cfg(windows)]
        {
            self.target.join("\\").is_dir()
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Some(parent) = self.target.parent() else {
                return false;
            };
            match (std::fs::metadata(&self.target), std::fs::metadata(parent)) {
                (Ok(target), Ok(parent)) => target.is_dir() && target.dev() != parent.dev(),
                _ => false,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    /// These counters describe rclone's LOCAL VFS cache, never cloud commit status.
    pub(crate) fn vfs_stats(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.rc("vfs/stats")?).context("invalid rclone VFS statistics")
    }

    pub(crate) fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|mut logs| logs.read_new())
            .unwrap_or_default()
    }

    pub(crate) fn is_running(&mut self) -> bool {
        !self.stopped && matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits up to `limit` for the owned rclone child to exit by itself and never
    /// kills it. A live macOS NFS server may still answer kernel I/O through the
    /// WebDAV backend, so the caller keeps that backend serving until this returns true.
    pub(crate) fn wait_for_exit(&mut self, limit: Duration) -> Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            match self.child.try_wait()? {
                Some(status) => {
                    self.stopped = true;
                    // Same criteria as a timely graceful stop; otherwise the lease stays.
                    if self.graceful_quit_requested && status.success() {
                        self.lease.clear_if_unmounted(&self.target)?;
                        self.shutdown_uncertain = false;
                    }
                    return Ok(true);
                }
                None if Instant::now() >= deadline => return Ok(false),
                None => thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    pub(crate) fn stop(&mut self) -> Result<StopReport> {
        self.stop_with_grace(Duration::from_secs(if cfg!(target_os = "macos") {
            12
        } else {
            3
        }))
    }

    fn stop_with_grace(&mut self, grace: Duration) -> Result<StopReport> {
        if self.shutdown_uncertain {
            bail!("macOS NFS shutdown remains uncertain; rclone was not killed and mount lease is retained");
        }
        if self.stopped {
            self.lease.clear_if_unmounted(&self.target)?;
            return Ok(StopReport {
                forced: false,
                cache_preserved: true,
            });
        }
        if self.poll()?.is_some() {
            return Ok(StopReport {
                forced: false,
                cache_preserved: true,
            });
        }
        // Deliver delayed saves to the still-serving WebDAV backend before rclone quits.
        match drain_writeback(self.address, &self.credential, WRITEBACK_DRAIN_LIMIT) {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "Native saves were still queued after {}s; they remain in the durable VFS cache for the next start",
                WRITEBACK_DRAIN_LIMIT.as_secs()
            ),
            Err(error) => eprintln!(
                "Native write-back queue not drained ({error:#}); queued saves remain in the VFS cache"
            ),
        }
        self.graceful_quit_requested = self.rc("core/quit").is_ok();
        // macOS nfsmount must also tear down its loopback NFS server and native
        // mount. Allow its exit callback to complete before a forced kill.
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.poll()?.is_some() {
                return Ok(StopReport {
                    forced: false,
                    cache_preserved: true,
                });
            }
            thread::sleep(Duration::from_millis(50));
        }
        #[cfg(target_os = "macos")]
        {
            self.lease.record(&LeaseState::ShutdownUncertain {
                pid: self.child.id(),
            })?;
            self.shutdown_uncertain = true;
            bail!("macOS NFS unmount did not finish within the grace period; rclone was left alive to finish kernel I/O, mount lease retained. Do not reuse this workspace until the OS mount and I/O state are verified");
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.child
                .kill()
                .context("cannot stop mount process; mount may still be active")?;
            self.child.wait().context("cannot reap mount process")?;
            self.stopped = true;
            self.lease.clear_if_unmounted(&self.target)?;
            Ok(StopReport {
                forced: true,
                cache_preserved: true,
            })
        }
    }

    fn rc(&self, endpoint: &str) -> Result<Vec<u8>> {
        rc_call(
            self.address,
            &self.credential,
            endpoint,
            "{}",
            Duration::from_millis(400),
            1024 * 1024,
        )
    }
}

fn rc_call(
    address: SocketAddr,
    credential: &str,
    endpoint: &str,
    body: &str,
    timeout: Duration,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(stream, "POST /{endpoint} HTTP/1.0\r\nHost: {address}\r\nAuthorization: Basic {credential}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    let mut response = Vec::new();
    stream.take(limit).read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid mount control response")?;
    let headers = std::str::from_utf8(&response[..split])?;
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"))
    {
        bail!("mount control endpoint unavailable");
    }
    Ok(response[split + 4..].to_vec())
}

/// Makes rclone upload its delayed write-back queue now and waits until it is
/// empty. Returns false when `limit` expires. Items already retrying after an
/// upload error keep their backoff instead of being retried in a tight loop.
pub(super) fn drain_writeback(
    address: SocketAddr,
    credential: &str,
    limit: Duration,
) -> Result<bool> {
    let started = Instant::now();
    let mut next_note = started + Duration::from_secs(10);
    loop {
        let stats = rc_call(
            address,
            credential,
            "vfs/stats",
            "{}",
            Duration::from_secs(2),
            1024 * 1024,
        )?;
        let stats: serde_json::Value =
            serde_json::from_slice(&stats).context("invalid rclone VFS statistics")?;
        let Some(disk) = stats.get("diskCache") else {
            return Ok(true); // No VFS cache, so nothing can be queued.
        };
        let pending = disk["uploadsQueued"].as_u64().unwrap_or(0)
            + disk["uploadsInProgress"].as_u64().unwrap_or(0);
        if pending == 0 {
            return Ok(true);
        }
        if started.elapsed() >= limit {
            return Ok(false);
        }
        let queue = rc_call(
            address,
            credential,
            "vfs/queue",
            "{}",
            Duration::from_secs(2),
            8 * 1024 * 1024,
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        for item in queue
            .as_ref()
            .and_then(|queue| queue["queue"].as_array())
            .into_iter()
            .flatten()
        {
            let waiting = item["uploading"].as_bool() == Some(false)
                && item["tries"].as_u64().unwrap_or(0) == 0
                && item["expiry"].as_f64().is_some_and(|expiry| expiry > 0.0);
            if let (true, Some(id)) = (waiting, item["id"].as_i64()) {
                // An item that started uploading meanwhile just returns an error.
                let _ = rc_call(
                    address,
                    credential,
                    "vfs/queue-set-expiry",
                    &format!("{{\"id\":{id},\"expiry\":-1000000000}}"),
                    Duration::from_secs(2),
                    1024 * 1024,
                );
            }
        }
        if Instant::now() >= next_note {
            eprintln!(
                "Waiting for {pending} native saves to reach the RPool spool before unmounting"
            );
            next_note += Duration::from_secs(10);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// Recreates the private rclone log, keeping the previous session's log for
/// diagnosis. rclone does not log its RC credentials or the WebDAV bearer token;
/// display still redacts them and the file stays owner-only in `.rpool`.
fn open_mount_log(path: &Path) -> Result<std::fs::File> {
    reject_link(path)?;
    let previous = path.with_extension("previous.log");
    reject_link(&previous)?;
    if path.exists() {
        std::fs::rename(path, &previous).context("cannot keep previous rclone mount log")?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).context("cannot create rclone mount log")
}

/// Incrementally reads rclone's log file into bounded, redacted display lines.
struct MountLog {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    discarding: bool,
    secrets: Vec<String>,
}

impl MountLog {
    fn new(path: PathBuf, secrets: Vec<String>) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            discarding: false,
            secrets: secrets.into_iter().filter(|s| !s.is_empty()).collect(),
        }
    }

    fn read_new(&mut self) -> Vec<String> {
        let mut chunk = Vec::new();
        let read = std::fs::File::open(&self.path).and_then(|mut file| {
            if file.metadata()?.len() < self.offset {
                self.offset = 0;
                self.partial.clear();
                self.discarding = false;
            }
            file.seek(SeekFrom::Start(self.offset))?;
            file.take(LOG_READ_LIMIT).read_to_end(&mut chunk)
        });
        if read.is_err() {
            return Vec::new();
        }
        self.offset += chunk.len() as u64;
        let mut lines = Vec::new();
        let mut rest = &chunk[..];
        while let Some(end) = rest.iter().position(|&byte| byte == b'\n') {
            self.push(&rest[..end]);
            lines.push(self.finish_line());
            rest = &rest[end + 1..];
        }
        self.push(rest);
        if lines.len() > LOG_LINES_RETURNED {
            let omitted = lines.len() - LOG_LINES_RETURNED;
            lines.drain(..omitted);
            lines.insert(0, format!("[{omitted} earlier mount log lines omitted]"));
        }
        lines
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.discarding {
            return;
        }
        self.partial.extend_from_slice(bytes);
        if self.partial.len() > LOG_LINE_LIMIT {
            // Never publish a truncated fragment that might end inside a secret.
            self.partial.clear();
            self.discarding = true;
        }
    }

    fn finish_line(&mut self) -> String {
        if std::mem::take(&mut self.discarding) {
            return "[oversized mount log line omitted]".into();
        }
        let mut text = String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned();
        for secret in &self.secrets {
            text = text.replace(secret, "[redacted]");
        }
        text.trim_end().to_string()
    }
}

/// A volume label from a pool name: Windows labels hold at most 32 characters,
/// and separators or quotes could break the mount option string.
pub(super) fn volume_label(pool: &str) -> String {
    let label: String = pool
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    ',' | '\\' | '/' | ':' | '"' | '=' | '*' | '?' | '<' | '>' | '|'
                )
        })
        .take(32)
        .collect();
    let label = label.trim().to_string();
    if label.is_empty() {
        "RPool".into()
    } else {
        label
    }
}

/// The NFS server closes a VFS handle after each WRITE RPC. Without delayed
/// write-back, every close uploads the growing whole file through WebDAV.
pub(super) fn vfs_cache_policy(webdav: bool) -> (&'static str, &'static str) {
    if webdav {
        ("full", "60s")
    } else {
        ("writes", "0s")
    }
}

fn native_mount_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "nfsmount"
    } else {
        "mount"
    }
}

#[cfg(target_os = "macos")]
fn check_nfsmount_version(rclone: &str) -> Result<()> {
    let output = Command::new(rclone)
        .arg("version")
        .output()
        .context("cannot run rclone; macOS NFS mounting requires rclone v1.65 or newer")?;
    let first = String::from_utf8_lossy(&output.stdout);
    let version = first
        .lines()
        .next()
        .unwrap_or("")
        .strip_prefix("rclone v")
        .and_then(|v| {
            let mut parts = v.split('.');
            Some((
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse::<u32>().ok()?,
            ))
        });
    if !output.status.success()
        || !version.is_some_and(|(major, minor)| major > 1 || (major == 1 && minor >= 65))
    {
        bail!(
            "macOS NFS mounting requires rclone v1.65 or newer with nfsmount support; detected {}",
            first.lines().next().unwrap_or("unknown version")
        );
    }
    Ok(())
}

impl Drop for MountProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct MountIdentity {
    version: u32,
    source: PathBuf,
    cache: PathBuf,
    target: PathBuf,
    cache_mode: String,
    #[serde(default)]
    backend_identity: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum LeaseState {
    Launching,
    Running { pid: u32 },
    ShutdownUncertain { pid: u32 },
}

struct MountLease {
    path: PathBuf,
    // Serializes adapter startup even if a caller accidentally omits Workspace ownership.
    _lock: std::fs::File,
}

impl Drop for MountLease {
    fn drop(&mut self) {
        // On Unix a concurrently forked child can briefly retain this open file
        // description until exec, even with close-on-exec set. Closing only our
        // descriptor can therefore delay release past the lease lifetime.
        // The durable lease record independently fences uncertain/live mounts.
        let _ = self._lock.unlock();
    }
}

impl MountLease {
    fn prepare(
        source: &Path,
        cache: &Path,
        target: &Path,
        webdav: Option<&(String, String)>,
    ) -> Result<Self> {
        let metadata = source
            .parent()
            .context("workspace root missing")?
            .join(".rpool");
        let info =
            std::fs::symlink_metadata(&metadata).context("managed workspace metadata missing")?;
        if !info.is_dir() || info.file_type().is_symlink() {
            bail!("mount metadata must be a real directory");
        }
        let lock_path = metadata.join("mount-process.lock");
        reject_link(&lock_path)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .context("another mount adapter owns this workspace")?;
        let lease = Self {
            path: metadata.join("mount-process.json"),
            _lock: lock,
        };
        reject_link(&lease.path)?;
        if lease.path.exists() {
            let previous: LeaseState = serde_json::from_slice(&std::fs::read(&lease.path)?)
                .context("invalid mount lease; inspect surviving mount before manual recovery")?;
            match previous {
                LeaseState::Launching => bail!("mount launch outcome is uncertain; confirm no rclone process or driver mount uses this workspace before manually removing {} (retain all files/cache)", lease.path.display()),
                LeaseState::ShutdownUncertain { pid } => bail!("macOS NFS shutdown for PID {pid} is uncertain; inspect kernel I/O and mount state before manually clearing {} (retain all files/cache)", lease.path.display()),
                LeaseState::Running { pid } => {
                    if process_alive(pid).context("cannot prove previous mount process exited; lease retained")? {
                        bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
                    }
                    #[cfg(target_os = "macos")]
                    bail!("previous macOS NFS mount process PID {pid} exited without a confirmed clean shutdown; inspect kernel I/O and mount state before manually clearing {} (retain all files/cache)", lease.path.display());
                    // The recorded PID does not exist. Never signal/kill a possibly reused PID.
                    #[cfg(not(target_os = "macos"))]
                    lease.clear_if_unmounted(target)?;
                }
            }
        }
        #[cfg(windows)]
        let target = PathBuf::from(target.to_string_lossy().to_ascii_uppercase());
        #[cfg(not(windows))]
        let target = target.canonicalize()?;
        let identity = MountIdentity {
            version: 1,
            source: source.into(),
            cache: cache.into(),
            target,
            cache_mode: if webdav.is_some() { "full" } else { "writes" }.into(),
            backend_identity: webdav.map(|(url, token)| {
                blake3::hash(format!("webdav-other:{url}:{token}").as_bytes())
                    .to_hex()
                    .to_string()
            }),
        };
        let identity_path = metadata.join("mount-identity.json");
        reject_link(&identity_path)?;
        if identity_path.exists() {
            let previous: MountIdentity = serde_json::from_slice(&std::fs::read(identity_path)?)?;
            if previous != identity {
                bail!("mount source/cache/target identity changed; restart with the original workspace and mountpoint to preserve pending VFS writes");
            }
        } else {
            durable_json(&identity_path, &identity)?;
        }
        Ok(lease)
    }

    fn record(&self, state: &LeaseState) -> Result<()> {
        durable_json(&self.path, state)
    }

    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => sync_metadata_directory(self.path.parent().context("lease parent missing")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn clear_if_unmounted(&self, target: &Path) -> Result<()> {
        #[cfg(target_os = "macos")]
        if macos_mount_attached(target)? {
            bail!(
                "native mount remains attached at {}; mount lease retained",
                target.display()
            );
        }
        #[cfg(not(target_os = "macos"))]
        let _ = target;
        self.clear()
    }
}

#[cfg(target_os = "macos")]
fn macos_mount_attached(target: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn getmntinfo_r_np(mntbufp: *mut *mut libc::statfs, flags: libc::c_int) -> libc::c_int;
    }
    let mut table: *mut libc::statfs = std::ptr::null_mut();
    // MNT_NOWAIT avoids querying an unreachable NFS server. The _r_np variant
    // owns a fresh buffer per call, unlike getmntinfo's shared static buffer.
    let count = unsafe { getmntinfo_r_np(&mut table, libc::MNT_NOWAIT) };
    if count <= 0 || table.is_null() {
        if !table.is_null() {
            unsafe { libc::free(table.cast()) };
        }
        bail!("cannot verify macOS mount table; mount lease retained");
    }
    let mounts = unsafe { std::slice::from_raw_parts(table, count as usize) };
    let target = target.as_os_str().as_bytes();
    let attached = mounts.iter().any(|mount| {
        let name = &mount.f_mntonname;
        let len = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        name[..len]
            .iter()
            .map(|&byte| byte as u8)
            .eq(target.iter().copied())
    });
    unsafe { libc::free(table.cast()) };
    Ok(attached)
}

#[cfg(all(test, target_os = "macos"))]
mod mount_table_tests {
    use super::*;

    #[test]
    fn attached_mount_retains_lease_until_kernel_table_clears() {
        let root = tempfile::tempdir().unwrap();
        let lease = MountLease {
            path: root.path().join("mount-process.json"),
            _lock: std::fs::File::create(root.path().join("mount-process.lock")).unwrap(),
        };
        lease.record(&LeaseState::Running { pid: 1 }).unwrap();
        assert!(macos_mount_attached(Path::new("/")).unwrap());
        assert!(lease.clear_if_unmounted(Path::new("/")).is_err());
        assert!(lease.path.exists());
        assert!(!macos_mount_attached(root.path()).unwrap());
        lease.clear_if_unmounted(root.path()).unwrap();
        assert!(!lease.path.exists());
    }
}

fn reject_link(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("mount state must be a regular file: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn durable_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().context("mount metadata parent missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_metadata_directory(parent)
}

fn sync_metadata_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> Result<bool> {
    let pid = i32::try_from(pid).context("invalid recorded mount PID")?;
    if pid <= 0 {
        bail!("invalid recorded mount PID");
    }
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // Signal zero performs existence/access checking only; it never sends a signal.
    if unsafe { kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        Ok(false)
    } else {
        Err(error.into())
    }
}

#[cfg(windows)]
pub(crate) fn process_alive(pid: u32) -> Result<bool> {
    if pid == 0 {
        bail!("invalid recorded mount PID");
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) }; // SYNCHRONIZE
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == 87 {
            return Ok(false);
        } // nonexistent process: ERROR_INVALID_PARAMETER
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    let result = unsafe { WaitForSingleObject(handle, 0) };
    let error = if result == 0xffff_ffff {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        CloseHandle(handle);
    }
    match result {
        0 => Ok(false),  // process handle signaled: exited
        258 => Ok(true), // WAIT_TIMEOUT
        _ => Err(error
            .unwrap_or_else(|| std::io::Error::other("unexpected process wait result"))
            .into()),
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn process_alive(_: u32) -> Result<bool> {
    bail!("process liveness unavailable on this platform")
}

pub(crate) fn validate_mountpoint(config: &MountConfig) -> Result<()> {
    if !config.files_dir.is_absolute() || !config.cache_dir.is_absolute() {
        bail!("mount source and cache must be absolute paths");
    }
    let source = config
        .files_dir
        .canonicalize()
        .context("mount source directory does not exist")?;
    if !source.is_dir() {
        bail!("mount source must be a directory");
    }
    let cache = normalized_existing_or_parent(&config.cache_dir)?;
    if source.starts_with(&cache) || cache.starts_with(&source) {
        bail!("mount source and VFS cache must not overlap");
    }
    #[cfg(windows)]
    {
        let value = config.target.to_string_lossy();
        let bytes = value.as_bytes();
        if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            bail!("Windows mount target must be an unused drive letter, such as R:");
        }
        // Unlike Path::exists, GetLogicalDrives also sees inaccessible/removable drives.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLogicalDrives() -> u32;
        }
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            bail!("cannot determine occupied Windows drive letters");
        }
        let index = bytes[0].to_ascii_uppercase() - b'A';
        if mask & (1u32 << index) != 0 {
            bail!("mount drive letter is already in use");
        }
    }
    #[cfg(not(windows))]
    {
        if !config.target.is_absolute() {
            bail!("mount target must be an absolute directory");
        }
        let metadata =
            std::fs::symlink_metadata(&config.target).context("mount target must already exist")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("mount target must be a real directory, not a symlink");
        }
        let target = config.target.canonicalize()?;
        let workspace = source
            .parent()
            .context("mount source needs a workspace parent")?;
        if target.starts_with(workspace) || workspace.starts_with(&target) {
            bail!("mount target must be outside the entire workspace, including metadata");
        }
        if target != config.target {
            bail!("mount target must use its canonical path without symlink components");
        }
        if target.starts_with(&source)
            || source.starts_with(&target)
            || target.starts_with(&cache)
            || cache.starts_with(&target)
        {
            bail!("mount target must not overlap source or cache");
        }
        if std::fs::read_dir(&target)?.next().is_some() {
            bail!("mount target directory must be empty");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Some(parent) = target.parent() {
                if metadata.dev() != std::fs::metadata(parent)?.dev() {
                    bail!("mount target is already a filesystem mountpoint");
                }
            }
        }
    }
    Ok(())
}

fn normalized_existing_or_parent(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path
        .parent()
        .context("cache needs a parent directory")?
        .canonicalize()?;
    let name = path.file_name().context("cache needs a directory name")?;
    Ok(parent.join(name))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn dav_vfs_policy_keeps_dirty_writes_until_after_the_nfs_callback_burst() {
        assert_eq!(vfs_cache_policy(true), ("full", "60s"));
        assert_eq!(vfs_cache_policy(false), ("writes", "0s"));
    }

    fn lease_fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let files = root.path().join("files");
        let cache = root.path().join("cache");
        std::fs::create_dir(&files).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::create_dir(root.path().join(".rpool")).unwrap();
        #[cfg(windows)]
        let target = PathBuf::from("R:");
        #[cfg(not(windows))]
        let target = other.path().canonicalize().unwrap();
        (
            root,
            other,
            files.canonicalize().unwrap(),
            cache.canonicalize().unwrap(),
            target,
        )
    }

    #[test]
    fn lease_survives_owner_drop_and_uncertain_launch_blocks_restart() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
        lease.record(&LeaseState::Launching).unwrap();
        let path = lease.path.clone();
        drop(lease);
        assert!(path.exists());
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    }

    #[test]
    fn active_pid_blocks_restart_without_sending_signal() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        lease
            .record(&LeaseState::Running {
                pid: std::process::id(),
            })
            .unwrap();
        drop(lease);
        assert!(process_alive(std::process::id()).unwrap());
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
        assert!(process_alive(0).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn uncertain_nfs_shutdown_never_kills_child_or_clears_lease() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        let child = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        lease.record(&LeaseState::Running { pid }).unwrap();
        let lease_path = lease.path.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut process = MountProcess {
            child,
            target: target.clone(),
            address,
            credential: "synthetic".into(),
            logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: false,
            lease,
        };
        assert!(process.stop_with_grace(Duration::from_millis(1)).is_err());
        assert!(process.shutdown_uncertain);
        assert!(process.child.try_wait().unwrap().is_none());
        drop(process);
        assert!(process_alive(pid).unwrap());
        assert!(lease_path.exists());
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
            libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
        }
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unexpected_nfs_child_exit_retains_lease_even_without_mount_table_entry() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        let child = Command::new("/usr/bin/true").spawn().unwrap();
        lease
            .record(&LeaseState::Running { pid: child.id() })
            .unwrap();
        let path = lease.path.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut process = MountProcess {
            child,
            target: target.clone(),
            address,
            credential: "synthetic".into(),
            logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: false,
            lease,
        };
        process.child.wait().unwrap();
        assert!(process.poll().is_err());
        drop(process);
        assert!(path.exists());
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nfsmount_minimum_version_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("old-rclone");
        std::fs::write(&script, "#!/bin/sh\necho 'rclone v1.64.0'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_nfsmount_version(script.to_str().unwrap()).is_err());
        std::fs::write(&script, "#!/bin/sh\necho 'rclone v1.65.0'\n").unwrap();
        assert!(check_nfsmount_version(script.to_str().unwrap()).is_ok());
    }

    #[test]
    fn dropping_lease_unlocks_even_with_an_inherited_description() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        // Deterministically model the descriptor inherited across fork without
        // depending on thread/process scheduling or weakening exclusive locking.
        let inherited = lease._lock.try_clone().unwrap();
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
        drop(lease);
        let next = MountLease::prepare(&files, &cache, &target, None).unwrap();
        assert!(MountLease::prepare(&files, &cache, &target, None).is_err());
        drop(next);
        drop(inherited);
    }

    #[test]
    fn identity_is_stable_after_clean_stop() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        lease.record(&LeaseState::Launching).unwrap();
        lease.clear().unwrap();
        drop(lease);
        drop(MountLease::prepare(&files, &cache, &target, None).unwrap());
        let different_cache = cache.parent().unwrap().join("other-cache");
        std::fs::create_dir(&different_cache).unwrap();
        assert!(MountLease::prepare(&files, &different_cache, &target, None).is_err());
        #[cfg(windows)]
        let different_target = PathBuf::from("S:");
        #[cfg(not(windows))]
        let different_target = tempfile::tempdir().unwrap();
        #[cfg(not(windows))]
        let different_target_path = different_target.path();
        #[cfg(windows)]
        let different_target_path = different_target.as_path();
        assert!(MountLease::prepare(&files, &cache, different_target_path, None).is_err());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn forced_stop_reaps_owned_child_and_preserves_cache() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let sentinel = cache.join("pending-write");
        std::fs::write(&sentinel, b"must survive").unwrap();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        lease.record(&LeaseState::Launching).unwrap();
        // Synthetic local child only: no rclone, driver, cloud or configuration access.
        let child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 60"])
            .spawn()
            .unwrap();
        let pid = child.id();
        lease.record(&LeaseState::Running { pid }).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let lease_path = lease.path.clone();
        let mut process = MountProcess {
            child,
            target,
            address,
            credential: "synthetic".into(),
            logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: false,
            lease,
        };
        let report = process.stop().unwrap();
        assert!(report.forced);
        assert!(report.cache_preserved);
        assert!(process.child.try_wait().unwrap().is_some());
        assert!(!lease_path.exists());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"must survive");
    }

    #[test]
    fn mount_log_tails_incrementally_redacts_and_bounds_output() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rclone-mount.log");
        let mut log = MountLog::new(path.clone(), vec!["s3cret".into(), String::new()]);
        assert!(log.read_new().is_empty()); // Missing file is not an error.
        std::fs::write(&path, b"first s3cret line\npartial").unwrap();
        assert_eq!(log.read_new(), ["first [redacted] line"]);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b" continued\n").unwrap();
        assert_eq!(log.read_new(), ["partial continued"]);
        file.write_all(&vec![b'x'; LOG_LINE_LIMIT + 10]).unwrap();
        assert!(log.read_new().is_empty());
        file.write_all(b"tail-s3cret\nnext\n").unwrap();
        assert_eq!(
            log.read_new(),
            ["[oversized mount log line omitted]", "next"]
        );
        let many: String = (0..LOG_LINES_RETURNED + 5)
            .map(|i| format!("{i}\n"))
            .collect();
        file.write_all(many.as_bytes()).unwrap();
        let lines = log.read_new();
        assert_eq!(lines.len(), LOG_LINES_RETURNED + 1);
        assert_eq!(lines[0], "[5 earlier mount log lines omitted]");
        assert_eq!(lines.last().unwrap(), &(LOG_LINES_RETURNED + 4).to_string());
    }

    #[test]
    fn mount_log_is_private_and_keeps_previous_session() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rclone-mount.log");
        std::fs::write(&path, b"old session\n").unwrap();
        let mut file = open_mount_log(&path).unwrap();
        file.write_all(b"new session\n").unwrap();
        assert_eq!(
            std::fs::read(root.path().join("rclone-mount.previous.log")).unwrap(),
            b"old session\n"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"new session\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let link = root.path().join("linked.log");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(open_mount_log(&link).is_err());
        }
    }

    /// Minimal authenticated rclone RC stand-in: answers vfs/stats, vfs/queue and
    /// records vfs/queue-set-expiry calls.
    fn fake_rc(
        queued: Arc<std::sync::atomic::AtomicU64>,
        tries: u64,
    ) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        use std::sync::atomic::Ordering;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    request.extend_from_slice(&buffer[..n]);
                    if n == 0 || request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let head = text.split("\r\n\r\n").next().unwrap_or("").to_string();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let mut body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                while body.len() < length {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    body.push_str(&String::from_utf8_lossy(&buffer[..n]));
                }
                let authorized = head.contains("Authorization: Basic cnBvb2w6cGFzc3dvcmQ=");
                let endpoint = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.lock().unwrap().push(format!("{endpoint} {body}"));
                let reply = match (authorized, endpoint.as_str()) {
                    (false, _) => None,
                    (true, "/vfs/stats") => Some(format!(
                        "{{\"diskCache\":{{\"uploadsQueued\":{},\"uploadsInProgress\":0}}}}",
                        queued.load(Ordering::SeqCst)
                    )),
                    (true, "/vfs/queue") => Some(format!(
                        "{{\"queue\":[{{\"id\":7,\"uploading\":false,\"tries\":{tries},\"expiry\":59.5}}]}}"
                    )),
                    (true, "/vfs/queue-set-expiry") => {
                        queued.store(0, Ordering::SeqCst);
                        Some("{}".into())
                    }
                    _ => None,
                };
                let response = match reply {
                    Some(json) => format!(
                        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{json}",
                        json.len()
                    ),
                    None => "HTTP/1.0 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into(),
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (address, calls)
    }

    #[test]
    fn drain_expedites_delayed_writeback_until_queue_is_empty() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let credential = base64(b"rpool:password");
        let queued = Arc::new(AtomicU64::new(1));
        let (address, calls) = fake_rc(queued.clone(), 0);
        assert!(drain_writeback(address, &credential, Duration::from_secs(10)).unwrap());
        assert_eq!(queued.load(Ordering::SeqCst), 0);
        let calls = calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|c| c == "/vfs/queue-set-expiry {\"id\":7,\"expiry\":-1000000000}"));
    }

    #[test]
    fn drain_respects_retry_backoff_and_time_limit() {
        use std::sync::atomic::AtomicU64;
        let credential = base64(b"rpool:password");
        let (address, calls) = fake_rc(Arc::new(AtomicU64::new(3)), 2);
        let started = Instant::now();
        assert!(!drain_writeback(address, &credential, Duration::from_millis(300)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("/vfs/queue-set-expiry")));
        // Wrong credentials or no RC server are reported, not treated as drained.
        assert!(drain_writeback(address, "wrong", Duration::from_secs(1)).is_err());
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(drain_writeback(closed, &credential, Duration::from_secs(1)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn late_graceful_exit_is_awaited_without_killing_and_then_clears_lease() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 0.5"])
            .spawn()
            .unwrap();
        lease
            .record(&LeaseState::ShutdownUncertain { pid: child.id() })
            .unwrap();
        let lease_path = lease.path.clone();
        let mut process = MountProcess {
            child,
            target,
            address: TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap(),
            credential: "synthetic".into(),
            logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
            stopped: false,
            graceful_quit_requested: true,
            shutdown_uncertain: true,
            lease,
        };
        assert!(process.is_running());
        assert!(!process.wait_for_exit(Duration::from_millis(50)).unwrap());
        assert!(process.is_running(), "waiting must never kill the child");
        assert!(process.wait_for_exit(Duration::from_secs(10)).unwrap());
        assert!(!process.is_running());
        assert!(!process.shutdown_uncertain);
        assert!(!lease_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn late_exit_without_graceful_quit_keeps_uncertain_lease() {
        let (_root, _other, files, cache, target) = lease_fixture();
        let lease = MountLease::prepare(&files, &cache, &target, None).unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        lease
            .record(&LeaseState::ShutdownUncertain { pid: child.id() })
            .unwrap();
        let lease_path = lease.path.clone();
        let mut process = MountProcess {
            child,
            target,
            address: TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap(),
            credential: "synthetic".into(),
            logs: Mutex::new(MountLog::new(PathBuf::from("unused-mount.log"), Vec::new())),
            stopped: false,
            graceful_quit_requested: false,
            shutdown_uncertain: true,
            lease,
        };
        assert!(process.wait_for_exit(Duration::from_secs(10)).unwrap());
        assert!(process.shutdown_uncertain);
        assert!(lease_path.exists());
        drop(process);
        assert!(lease_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rclone_output_goes_to_private_log_file_not_pipes() {
        use std::os::unix::fs::PermissionsExt;
        let (root, _other, files, cache, target) = lease_fixture();
        let script = root.path().join("fake rclone");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'rclone v1.75.1'; exit 0; fi\n\
             echo \"stdout pass=$RCLONE_RC_PASS\"\n\
             for a in \"$@\"; do [ \"$prev\" = --volname ] && echo \"volname=$a\"; prev=\"$a\"; done\n\
             echo \"stderr token=$RCLONE_WEBDAV_BEARER_TOKEN\" >&2\n\
             [ -p /dev/stdout ] && echo 'stdout is a pipe'\n\
             exec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut process = MountProcess::start(MountConfig {
            rclone: script.to_str().unwrap().into(),
            files_dir: files,
            cache_dir: cache,
            target,
            shared: false,
            read_only: false,
            vfs_cache_gib: 1,
            cache_min_free_gib: 0,
            webdav: Some(("http://127.0.0.1:9/".into(), "bearer-secret-token".into())),
            volume_name: Some("My Pool".into()),
        })
        .unwrap();
        let mut lines = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while lines.len() < 3 && Instant::now() < deadline {
            lines.extend(process.logs());
            thread::sleep(Duration::from_millis(50));
        }
        let log_path = root.path().join(".rpool/rclone-mount.log");
        let mode = std::fs::metadata(&log_path).unwrap().permissions().mode();
        process.child.kill().unwrap();
        process.child.wait().unwrap();
        process.stopped = true;
        drop(process);
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            lines.contains(&"stdout pass=[redacted]".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"stderr token=[redacted]".to_string()),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("pipe")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("bearer-secret-token")));
        assert!(lines.contains(&"volname=My Pool".to_string()), "{lines:?}");
    }

    #[test]
    fn volume_labels_are_the_pool_name_made_safe() {
        assert_eq!(volume_label("archive"), "archive");
        assert_eq!(volume_label("My Pool"), "My Pool");
        assert_eq!(volume_label("a,b:c/d\\e\"f"), "abcdef");
        assert_eq!(volume_label(&"x".repeat(40)).len(), 32);
        assert_eq!(volume_label("사진 보관"), "사진 보관");
        assert_eq!(volume_label(",,,"), "RPool");
    }

    #[test]
    fn encodes_basic_auth() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"rpool:password"), "cnBvb2w6cGFzc3dvcmQ=");
    }
    #[cfg(unix)]
    #[test]
    fn rejects_overlap_nonempty_and_symlink_targets() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let files = root.join("files");
        let target_root = tempfile::tempdir().unwrap();
        let target = target_root.path().canonicalize().unwrap().join("target");
        std::fs::create_dir(&files).unwrap();
        std::fs::create_dir(&target).unwrap();
        let mut config = MountConfig {
            rclone: "rclone".into(),
            files_dir: files.clone(),
            cache_dir: root.join("cache"),
            target: target.clone(),
            shared: false,
            read_only: false,
            vfs_cache_gib: 10,
            cache_min_free_gib: 2,
            webdav: None,
            volume_name: None,
        };
        assert!(validate_mountpoint(&config).is_ok());
        let metadata = root.join(".rpool/archives");
        std::fs::create_dir_all(&metadata).unwrap();
        config.target = metadata;
        assert!(validate_mountpoint(&config).is_err());
        config.target = target.clone();
        config.cache_dir = files.join("cache");
        assert!(validate_mountpoint(&config).is_err());
        config.cache_dir = root.join("cache");
        std::fs::write(target.join("occupied"), b"x").unwrap();
        assert!(validate_mountpoint(&config).is_err());
        let link = root.join("link");
        std::os::unix::fs::symlink(&files, &link).unwrap();
        config.target = link;
        assert!(validate_mountpoint(&config).is_err());
    }
}

/// Called before reconnecting an existing DAV endpoint to an orphaned rclone.
pub(crate) fn preflight_virtual(root: &Path) -> Result<()> {
    let path = root.join(".rpool/mount-process.json");
    reject_link(&path)?;
    if path.exists() {
        let lease: LeaseState = serde_json::from_slice(&std::fs::read(path)?)?;
        match lease {
            LeaseState::Launching => {
                bail!("uncertain previous mount launch; preserve cache and inspect process")
            }
            LeaseState::Running { pid } if process_alive(pid)? => {
                bail!("previous mount PID {pid} is still active")
            }
            _ => {}
        }
    }
    Ok(())
}

// rclone owns native cache eviction: never remove dirty/open native files ourselves.
fn configure_cache(command: &mut Command, limit_gib: u64, min_free_gib: u64) {
    command.args([
        "--vfs-cache-max-size",
        &format!("{limit_gib}G"),
        "--vfs-cache-min-free-space",
        &format!("{min_free_gib}G"),
        "--vfs-cache-poll-interval",
        "5s",
    ]);
}

#[cfg(test)]
mod cache_option_tests {
    use super::*;
    #[test]
    fn forwards_bounded_native_cache_and_disk_headroom_without_touching_files() {
        let mut command = Command::new("unused");
        configure_cache(&mut command, 7, 3);
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--vfs-cache-max-size",
                "7G",
                "--vfs-cache-min-free-space",
                "3G",
                "--vfs-cache-poll-interval",
                "5s"
            ]
        );
    }
}
