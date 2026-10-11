//! JSON contract of the monitoring files (shared by mount, CLI and GUI).
use serde::{Deserialize, Serialize};

/// Current [`NetStatus::version`].
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
    /// Pool being mounted.
    pub pool: String,
    /// Absolute workspace path of the mount.
    pub workspace: String,
    /// Drive letter or mount path.
    pub mountpoint: String,
    /// `fuse`, `winfsp` or `dav`.
    pub frontend: String,
    /// Mount process id.
    pub pid: u32,
    /// Mount start, Unix seconds.
    pub started_unix: u64,
}

/// Live traffic of one mount; rewritten about once per second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NetStatus {
    /// Format version ([`STATUS_VERSION`]).
    pub version: u32,
    /// Pool being mounted.
    pub pool: String,
    /// When this status was written, Unix seconds (stale after
    /// `files::STATUS_STALE_SECONDS`).
    pub updated_unix: u64,
    /// Since the mount started.
    pub uptime_seconds: u64,
    /// Traffic per pool remote.
    pub remotes: Vec<RemoteTraffic>,
    /// Pending local changes.
    pub queue: QueueStatus,
    /// Last successful publish of this pool's metadata to the cloud.
    pub last_sync_unix: Option<u64>,
    /// Active alerts (stall, errors, unreachable, metadata, upload limit).
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Traffic counters of one pool remote since the mount started.
pub(crate) struct RemoteTraffic {
    /// Remote address as in the pool (e.g. `dropbox_1_crypt:`).
    pub remote: String,
    /// Bottom rclone backend type (e.g. `dropbox`); `None` until resolved.
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
    /// 10 s average upload rate, bytes/s.
    pub upload_rate_10s: f64,
    /// 1 s download rate, bytes/s.
    pub download_rate_1s: f64,
    /// 10 s average download rate, bytes/s.
    pub download_rate_10s: f64,
    /// Uploads in progress.
    pub active_uploads: u32,
    /// Downloads in progress.
    pub active_downloads: u32,
    /// Successful operations.
    pub ok_ops: u64,
    /// Failed operations.
    pub failed_ops: u64,
    /// Provider-rejected writes retried with backoff (rate limits).
    pub retries: u64,
    /// Last successful operation, Unix seconds.
    pub last_ok_unix: Option<u64>,
    /// Last error, one line, no secrets.
    pub last_error: Option<String>,
    /// Time of `last_error`, Unix seconds.
    pub last_error_unix: Option<u64>,
    /// Last failed upload, independently of read/listing errors.
    #[serde(default)]
    pub last_upload_error: Option<String>,
    #[serde(default)]
    pub last_upload_error_unix: Option<u64>,
    /// The last failed upload had explicit storage-capacity evidence.
    #[serde(default)]
    pub last_upload_storage_full: bool,
    /// Quota preflight found no upload space at this target.
    #[serde(default)]
    pub capacity_no_space_unix: Option<u64>,
}

/// Local changes not yet in the cloud.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueueStatus {
    /// Pending changes (writes and deletions, including multiple revisions of a file).
    pub pending_files: u64,
    /// Bytes waiting for upload.
    pub pending_bytes: u64,
    /// Change time of the oldest pending file, Unix seconds.
    pub oldest_pending_unix: Option<u64>,
    /// `None` from an older mount that does not expose individual changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<QueueItem>>,
}

/// One durable pending change, in namespace acknowledgement order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct QueueItem {
    /// Stable intent identity; a path can have several pending revisions.
    pub id: String,
    /// Relative path shown on the mounted drive.
    pub path: String,
    /// A write has spool bytes; a deletion has none.
    pub operation: QueueOperation,
    pub size: u64,
    pub since_unix: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QueueOperation {
    Write,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Kind of a monitoring alert; the GUI translates by kind.
pub(crate) enum AlertKind {
    /// Pending uploads but no upload traffic for `STALL_SECONDS`.
    Stalled,
    /// Failed operations increased in the last minute.
    Errors,
    /// Every operation on an account failed for a minute or more.
    Unreachable,
    /// Pool metadata records not covered by a checkpoint approach the old
    /// bootstrap limit, or automatic compaction failed.
    MetadataGrowing,
    /// Uploads to the account wait for its daily upload limit (the
    /// message says when they resume).
    UploadLimit,
    /// A named storage reported capacity exhaustion on its last upload.
    StorageFull,
    /// The named storage had no available shard-upload budget at preflight.
    NoUploadSpace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One active alert in the status file.
pub(crate) struct Alert {
    /// What is wrong.
    pub kind: AlertKind,
    /// `None` for pool-wide alerts (e.g. stalled queue).
    pub remote: Option<String>,
    /// Since when the condition holds, Unix seconds.
    pub since_unix: u64,
    /// English one-liner (the GUI shows its own translated text by `kind`).
    pub message: String,
}

/// One account's traffic in one minute (history line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HistoryPoint {
    /// Start of the minute, Unix seconds.
    pub minute_unix: u64,
    /// Pool remote address.
    pub remote: String,
    /// Bytes uploaded in that minute.
    pub upload_bytes: u64,
    /// Bytes downloaded in that minute.
    pub download_bytes: u64,
    /// Operations that succeeded in that minute.
    pub ok_ops: u64,
    /// Operations that failed in that minute.
    pub failed_ops: u64,
}
