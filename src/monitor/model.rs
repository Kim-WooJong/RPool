//! JSON contract of the monitoring files (shared by mount, CLI and GUI).
use serde::{Deserialize, Serialize};

pub(crate) const STATUS_VERSION: u32 = 1;
/// Live status file under `<workspace>/.rpool/`.
pub(crate) const STATUS_FILE: &str = "net-status.json";
/// History folder under `<workspace>/.rpool/`.
pub(crate) const HISTORY_DIR: &str = "net-history";
/// History files older than this are deleted.
pub(crate) const HISTORY_DAYS: u64 = 90;
/// Registry folder under the app config dir.
pub(crate) const REGISTRY_DIR: &str = "mounts";
/// Default: pending uploads but no upload traffic for this long = stalled.
pub(crate) const STALL_SECONDS: u64 = 180;

/// One running mount, registered while its process lives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MountEntry {
    /// Random id of this mount process run (registry file name).
    pub id: String,
    pub pool: String,
    pub workspace: String,
    /// Drive letter or mount path.
    pub mountpoint: String,
    /// `fuse`, `winfsp` or `dav`.
    pub frontend: String,
    pub pid: u32,
    pub started_unix: u64,
}

/// Live traffic of one mount; rewritten about once per second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NetStatus {
    pub version: u32,
    pub pool: String,
    pub updated_unix: u64,
    /// Since the mount started.
    pub uptime_seconds: u64,
    pub remotes: Vec<RemoteTraffic>,
    pub queue: QueueStatus,
    /// Last successful publish of this pool's metadata to the cloud.
    pub last_sync_unix: Option<u64>,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteTraffic {
    /// Remote address as in the pool (e.g. `dropbox_1_crypt:`).
    pub remote: String,
    pub backend: Option<String>,
    /// Bytes handed to rclone for uploads since the mount started.
    pub sent_bytes: u64,
    /// Of those, bytes whose upload the provider acknowledged (rclone exited
    /// successfully).
    pub acked_bytes: u64,
    /// Of those, bytes read back and verified.
    pub verified_bytes: u64,
    /// Bytes downloaded (reads, readbacks, metadata).
    pub received_bytes: u64,
    /// Bytes per second over the last 1 s and 10 s.
    pub upload_rate_1s: f64,
    pub upload_rate_10s: f64,
    pub download_rate_1s: f64,
    pub download_rate_10s: f64,
    pub active_uploads: u32,
    pub active_downloads: u32,
    pub ok_ops: u64,
    pub failed_ops: u64,
    /// Provider-rejected writes retried with backoff (rate limits).
    pub retries: u64,
    pub last_ok_unix: Option<u64>,
    /// Last error, one line, no secrets.
    pub last_error: Option<String>,
    pub last_error_unix: Option<u64>,
}

/// Local changes not yet in the cloud.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueueStatus {
    pub pending_files: u64,
    pub pending_bytes: u64,
    pub oldest_pending_unix: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AlertKind {
    /// Pending uploads but no upload traffic for `STALL_SECONDS`.
    Stalled,
    /// Failed operations increased in the last minute.
    Errors,
    /// Every operation on an account failed for a minute or more.
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Alert {
    pub kind: AlertKind,
    /// `None` for pool-wide alerts (e.g. stalled queue).
    pub remote: Option<String>,
    pub since_unix: u64,
    /// English one-liner (the GUI shows its own translated text by `kind`).
    pub message: String,
}

/// One account's traffic in one minute (history line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HistoryPoint {
    pub minute_unix: u64,
    pub remote: String,
    pub upload_bytes: u64,
    pub download_bytes: u64,
    pub ok_ops: u64,
    pub failed_ops: u64,
}
