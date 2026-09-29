//! Owned rclone mount process. Local/VFS data is deliberately never deleted here.
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) struct MountConfig {
    pub(crate) rclone: String,
    pub(crate) files_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) shared: bool,
    pub(crate) vfs_cache_gib: u64,
    pub(crate) cache_min_free_gib: u64,
    pub(crate) webdav: Option<(String, String)>,
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
    logs: Arc<Mutex<VecDeque<String>>>,
    stopped: bool,
    lease: MountLease,
}

impl MountProcess {
    pub(crate) fn start(config: MountConfig) -> Result<Self> {
        validate_mountpoint(&config)?;
        std::fs::create_dir_all(&config.cache_dir).context("cannot create durable VFS cache")?;
        let files = config.files_dir.canonicalize()?;
        let cache = config.cache_dir.canonicalize()?;
        let lease = MountLease::prepare(&files, &cache, &config.target, config.webdav.as_ref())?;
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
        let cache_mode = if config.webdav.is_some() {
            "full"
        } else {
            "writes"
        };
        let mut command = Command::new(&config.rclone);
        command
            .arg("mount")
            .arg(source)
            .arg(&config.target)
            .args([
                "--vfs-cache-mode",
                cache_mode,
                "--vfs-write-back",
                "0s",
                "--cache-dir",
            ])
            .arg(cache)
            .args(["--rc", "--rc-addr", &address.to_string()])
            .env("RCLONE_RC_USER", "rpool")
            .env("RCLONE_RC_PASS", &password)
            .env("RCLONE_RC_NO_AUTH", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
                return Err(error).context("cannot start rclone mount; install rclone and the platform filesystem driver (WinFsp on Windows, FUSE on Unix)");
            }
        };
        if let Err(error) = lease.record(&LeaseState::Running { pid: child.id() }) {
            // Do not discard a launching lease unless termination has actually been observed.
            if child.kill().is_ok() && child.wait().is_ok() {
                let _ = lease.clear();
            }
            return Err(error).context("cannot record mount child; uncertain lease retained unless child termination was confirmed");
        }
        let logs = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stdout) = child.stdout.take() {
            collect_log(
                stdout,
                logs.clone(),
                password.clone(),
                credential.clone(),
                config.webdav.as_ref().map(|(_, token)| token.clone()),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            collect_log(
                stderr,
                logs.clone(),
                password,
                credential.clone(),
                config.webdav.as_ref().map(|(_, token)| token.clone()),
            );
        }
        Ok(Self {
            child,
            target: config.target,
            address,
            credential,
            logs,
            stopped: false,
            lease,
        })
    }

    pub(crate) fn poll(&mut self) -> Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.stopped = true;
            self.lease.clear()?;
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
            .map(|mut logs| logs.drain(..).collect())
            .unwrap_or_default()
    }

    pub(crate) fn stop(&mut self) -> Result<StopReport> {
        if self.stopped || self.poll()?.is_some() {
            return Ok(StopReport {
                forced: false,
                cache_preserved: true,
            });
        }
        let _ = self.rc("core/quit");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.poll()?.is_some() {
                return Ok(StopReport {
                    forced: false,
                    cache_preserved: true,
                });
            }
            thread::sleep(Duration::from_millis(50));
        }
        self.child
            .kill()
            .context("cannot stop mount process; mount may still be active")?;
        self.child.wait().context("cannot reap mount process")?;
        self.stopped = true;
        self.lease.clear()?;
        Ok(StopReport {
            forced: true,
            cache_preserved: true,
        })
    }

    fn rc(&self, endpoint: &str) -> Result<Vec<u8>> {
        let timeout = Duration::from_millis(400);
        let mut stream = TcpStream::connect_timeout(&self.address, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        write!(stream, "POST /{endpoint} HTTP/1.0\r\nHost: {}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}", self.address, self.credential)?;
        let mut response = Vec::new();
        stream.take(1024 * 1024).read_to_end(&mut response)?;
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
                LeaseState::Running { pid } => {
                    if process_alive(pid).context("cannot prove previous mount process exited; lease retained")? {
                        bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
                    }
                    // The recorded PID does not exist. Never signal/kill a possibly reused PID.
                    lease.clear()?;
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
fn process_alive(pid: u32) -> Result<bool> {
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
fn process_alive(pid: u32) -> Result<bool> {
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
fn process_alive(_: u32) -> Result<bool> {
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

fn collect_log<R: Read + Send + 'static>(
    reader: R,
    logs: Arc<Mutex<VecDeque<String>>>,
    password: String,
    credential: String,
    bearer: Option<String>,
) {
    thread::spawn(move || {
        // Bound both individual line allocation and retained output.
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            let n = match Read::by_ref(&mut reader)
                .take(4096)
                .read_until(b'\n', &mut line)
            {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let oversized = n == 4096 && line.last() != Some(&b'\n');
            if oversized {
                // Do not publish truncated credential fragments; discard the whole line.
                loop {
                    let available = match reader.fill_buf() {
                        Ok(bytes) => bytes,
                        Err(_) => return,
                    };
                    if available.is_empty() {
                        break;
                    }
                    let end = available.iter().position(|b| *b == b'\n');
                    let count = end.map_or(available.len(), |i| i + 1);
                    reader.consume(count);
                    if end.is_some() {
                        break;
                    }
                }
            }
            let text = if oversized {
                "[oversized mount log line omitted]".to_string()
            } else {
                String::from_utf8_lossy(&line)
                    .replace(&password, "[redacted]")
                    .replace(&credential, "[redacted]")
            };
            let text = bearer
                .as_ref()
                .map_or(text.clone(), |token| text.replace(token, "[redacted]"));
            if let Ok(mut buffer) = logs.lock() {
                if buffer.len() >= 200 {
                    buffer.pop_front();
                }
                buffer.push_back(text.trim_end().to_string());
            }
        }
    });
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

    #[cfg(unix)]
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
            logs: Arc::new(Mutex::new(VecDeque::new())),
            stopped: false,
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
            vfs_cache_gib: 10,
            cache_min_free_gib: 2,
            webdav: None,
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
