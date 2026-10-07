//! Owned rclone mount process. Local/VFS data is deliberately never deleted here.
use anyhow::{bail, Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Durable mount-process lease and identity records.
mod lease;
/// Is a recorded mount PID still alive.
mod liveness;
/// Redacted, bounded rclone log tail.
mod mount_log;
/// Mountpoint validation and preflight.
mod mountpoint;
/// rclone mount command options.
mod options;
/// Stops a previous mount's rclone orphaned by a crashed RPool (unix; Windows
/// ends it with RPool through a job object).
#[cfg(unix)]
mod orphan;
/// Start, poll and stop the owned rclone child.
mod process;
/// rclone remote-control (RC) calls.
mod rc;
#[cfg(test)]
mod tests;

#[cfg(unix)]
use lease::sync_metadata_directory;
use lease::{reject_link, LeaseState, MountLease};
pub(crate) use liveness::process_alive;
use mount_log::{open_mount_log, MountLog};
pub(crate) use mountpoint::{preflight_virtual, validate_mountpoint};
#[cfg(target_os = "macos")]
use options::check_nfsmount_version;
use options::{configure_cache, kernel_mount_options, native_mount_command};
pub(in crate::mount) use options::{vfs_cache_policy, volume_label};
pub(in crate::mount) use rc::drain_writeback;
use rc::{base64, rc_call};

/// Upper bound for moving rclone's delayed write-back queue into the WebDAV
/// backend before quitting. Remaining items stay in the durable VFS cache.
const WRITEBACK_DRAIN_LIMIT: Duration = Duration::from_secs(90);
/// Longest log line kept (bytes); longer lines are replaced by a placeholder so a secret is never cut.
const LOG_LINE_LIMIT: usize = 4096;
/// Most log bytes read per `MountLog::read_new` call.
const LOG_READ_LIMIT: u64 = 1024 * 1024;
/// Most log lines returned per read; older ones are summarized as omitted.
const LOG_LINES_RETURNED: usize = 200;

/// rclone native mount of the drive's loopback WebDAV server.
pub(crate) struct MountConfig {
    /// rclone executable path.
    pub(crate) rclone: String,
    /// An existing directory directly inside the workspace root: it locates
    /// the workspace metadata (`.rpool/`, lease and log) and is never served.
    pub(crate) anchor_dir: PathBuf,
    /// Durable rclone VFS cache directory (`--cache-dir`); must not overlap the anchor or target.
    pub(crate) cache_dir: PathBuf,
    /// Mountpoint: empty absolute directory, or an unused drive letter such as `R:` on Windows.
    pub(crate) target: PathBuf,
    /// `--vfs-cache-max-size` in GiB.
    pub(crate) vfs_cache_gib: u64,
    /// `--vfs-cache-min-free-space` in GiB.
    pub(crate) cache_min_free_gib: u64,
    /// WebDAV URL and bearer token of the drive's server.
    pub(crate) webdav: (String, String),
    /// OS volume label (Explorer/Finder name); `None` keeps rclone's default.
    pub(crate) volume_name: Option<String>,
}

/// Outcome of [`MountProcess::stop`].
pub(crate) struct StopReport {
    /// rclone did not exit in the grace period and was killed (never on macOS).
    pub(crate) forced: bool,
}

/// Running rclone native mount (`mount`, or `nfsmount` on macOS) owned by this process.
/// Started by `virtual_drive::run`; stopped on drop.
pub(crate) struct MountProcess {
    /// rclone child process.
    child: Child,
    /// Mount target (canonical on macOS).
    target: PathBuf,
    /// Loopback address of rclone's RC server.
    address: SocketAddr,
    /// Base64 Basic-auth credential for RC calls.
    credential: String,
    /// Incremental reader of rclone's log file.
    logs: Mutex<MountLog>,
    /// Child exit has been observed.
    stopped: bool,
    /// `core/quit` was accepted, so an exit counts as graceful.
    graceful_quit_requested: bool,
    /// macOS unmount not confirmed; further stops refuse and the lease is retained.
    shutdown_uncertain: bool,
    /// Workspace mount lease held for the process lifetime.
    lease: MountLease,
}
