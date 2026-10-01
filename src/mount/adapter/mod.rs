//! Owned rclone mount process. Local/VFS data is deliberately never deleted here.
use anyhow::{bail, Context, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

mod lease;
mod liveness;
mod mount_log;
mod mountpoint;
mod options;
mod process;
mod rc;
#[cfg(test)]
mod tests;

use lease::{reject_link, LeaseState, MountLease};
pub(crate) use liveness::process_alive;
use mount_log::{open_mount_log, MountLog};
pub(crate) use mountpoint::{preflight_virtual, validate_mountpoint};
#[cfg(target_os = "macos")]
use options::check_nfsmount_version;
use options::{configure_cache, native_mount_command};
pub(in crate::mount) use options::{vfs_cache_policy, volume_label};
pub(in crate::mount) use rc::drain_writeback;
use rc::{base64, rc_call};

/// Upper bound for moving rclone's delayed write-back queue into the WebDAV
/// backend before quitting. Remaining items stay in the durable VFS cache.
const WRITEBACK_DRAIN_LIMIT: Duration = Duration::from_secs(90);
const LOG_LINE_LIMIT: usize = 4096;
const LOG_READ_LIMIT: u64 = 1024 * 1024;
const LOG_LINES_RETURNED: usize = 200;

/// rclone native mount of the drive's loopback WebDAV server.
pub(crate) struct MountConfig {
    pub(crate) rclone: String,
    /// An existing directory directly inside the workspace root: it locates
    /// the workspace metadata (`.rpool/`, lease and log) and is never served.
    pub(crate) anchor_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) vfs_cache_gib: u64,
    pub(crate) cache_min_free_gib: u64,
    /// WebDAV URL and bearer token of the drive's server.
    pub(crate) webdav: (String, String),
    /// OS volume label (Explorer/Finder name); `None` keeps rclone's default.
    pub(crate) volume_name: Option<String>,
}

pub(crate) struct StopReport {
    pub(crate) forced: bool,
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
