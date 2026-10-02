//! JSON contract of `rpool drive trash|versions|rollback --json` (CLI ↔ GUI).
use serde::{Deserialize, Serialize};

/// `version` field of every report in this contract.
pub(crate) const HISTORY_VERSION: u32 = 1;
/// Default days a deleted file stays in the trash.
pub(crate) const DEFAULT_TRASH_DAYS: u32 = 30;
/// Default number of previous versions kept per file (0 = unlimited).
pub(crate) const DEFAULT_KEEP_VERSIONS: u32 = 20;
/// Default days previous versions are kept (0 = unlimited).
pub(crate) const DEFAULT_VERSION_DAYS: u32 = 90;

/// Per-pool retention settings (portable, saved with the pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Retention {
    /// Days a deleted file stays in the trash (0 = unlimited).
    pub trash_days: u32,
    /// Previous versions kept per file (0 = unlimited).
    pub keep_versions: u32,
    /// Days previous versions are kept (0 = unlimited).
    pub version_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            trash_days: DEFAULT_TRASH_DAYS,
            keep_versions: DEFAULT_KEEP_VERSIONS,
            version_days: DEFAULT_VERSION_DAYS,
        }
    }
}

/// One deleted file (or folder) in the trash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrashEntry {
    /// Stable id for restore/purge (the deletion's revision/event id).
    pub id: String,
    /// Drive path at deletion time (`/Docs/a.txt`).
    pub path: String,
    /// Whether the entry is a folder.
    pub is_dir: bool,
    /// Bytes of the last version (folders: total of contained files).
    pub size: u64,
    /// When it was deleted (`None`: unknown time).
    pub deleted_unix: Option<u64>,
    /// PC (worker name) that deleted it.
    pub deleted_by: Option<String>,
    /// When it leaves the trash automatically (`None`: kept until purged).
    pub expires_unix: Option<u64>,
    /// A file now exists at the same path (restore would create a copy).
    pub path_taken: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// How a revision changed its file.
pub(crate) enum VersionKind {
    /// First revision of the file.
    Created,
    /// New content for an existing file.
    Modified,
    /// The file was deleted.
    Deleted,
    /// Content brought back from an earlier revision.
    Restored,
}

/// One revision of a file, newest first in listings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VersionEntry {
    /// Revision/event id (input of `versions restore`).
    pub id: String,
    /// Drive path of this revision (`/Docs/a.txt`).
    pub path: String,
    /// How this revision changed the file.
    pub kind: VersionKind,
    /// Bytes of this revision's content (0 for a deletion).
    pub size: u64,
    /// When the revision reached the cloud (`None`: unknown).
    pub time_unix: Option<u64>,
    /// PC (worker name) that wrote it.
    pub author: Option<String>,
    /// The version the drive shows now.
    pub current: bool,
    /// Data still available (not expired/purged), so it can be restored.
    pub restorable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// What a rollback does to one path.
pub(crate) enum ChangeAction {
    /// File existed at the target time and differs now: its old version comes back.
    Revert,
    /// File was deleted after the target time: it comes back.
    Undelete,
    /// File was created after the target time: it moves to the trash.
    Remove,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One planned rollback change.
pub(crate) struct RollbackChange {
    /// Drive path (`/Docs/a.txt`).
    pub path: String,
    /// What happens to the path.
    pub action: ChangeAction,
    /// Revision restored (Revert/Undelete) or removed (Remove).
    pub revision: String,
    /// Bytes of the affected revision.
    pub size: u64,
}

/// `rpool drive rollback` result (preview unless `applied`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RollbackPlan {
    /// Always `HISTORY_VERSION`.
    pub version: u32,
    /// Pool name.
    pub pool: String,
    /// Folder rolled back (`/` = whole drive).
    pub scope: String,
    /// Target time (unix seconds).
    pub at_unix: u64,
    /// Changes planned (or applied).
    pub changes: Vec<RollbackChange>,
    /// Paths that cannot be rolled back (data expired/purged), with reason.
    pub skipped: Vec<(String, String)>,
    /// Changes were applied (`--confirm`), not only previewed.
    pub applied: bool,
}

/// Default days a cleanup mark waits before its data is deleted.
pub(crate) const DEFAULT_CLEANUP_GRACE_DAYS: u32 = 7;

/// Per-pool automatic cleanup (portable, saved with the pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CleanupSettings {
    /// The mount's maintenance loop marks and deletes unreferenced data.
    pub auto: bool,
    /// Days between marking data and deleting it.
    pub grace_days: u32,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            auto: true,
            grace_days: DEFAULT_CLEANUP_GRACE_DAYS,
        }
    }
}

/// What a `rpool drive cleanup` run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanupMode {
    /// Nothing written (no `--confirm`).
    Preview,
    /// Marks published and/or due data deleted.
    Applied,
    /// Pending marks dropped (`--cancel`).
    Cancelled,
    /// A reference source could not be read: nothing was marked or deleted.
    Postponed,
}

/// Size of a set of drive archives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CleanupTotals {
    /// Archive folders (`virtual-*`, including incremental parts).
    pub archives: u64,
    /// File versions whose data they hold.
    pub files: u64,
    /// Stored objects (manifest replicas and shards).
    pub objects: u64,
    /// Stored bytes.
    pub bytes: u64,
}

/// Reclaimable data on one account (rclone remote).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CleanupAccount {
    /// rclone remote name.
    pub account: String,
    /// Objects on this account.
    pub objects: u64,
    /// Bytes on this account.
    pub bytes: u64,
}

/// `rpool drive cleanup --json` result (CLI ↔ GUI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CleanupReport {
    /// Always `HISTORY_VERSION`.
    pub version: u32,
    /// Pool name.
    pub pool: String,
    /// What the run did.
    pub mode: CleanupMode,
    /// Unreferenced data not marked yet (`--confirm` marks it).
    pub candidates: CleanupTotals,
    /// Marked, waiting for the grace period to end.
    pub waiting: CleanupTotals,
    /// Marked and past the grace period (`--confirm` deletes it after a
    /// fresh re-check), including interrupted deletions.
    pub due: CleanupTotals,
    /// Deleted by this run.
    pub deleted: CleanupTotals,
    /// Marks dropped by this run (data referenced again, or `--cancel`).
    pub released: u64,
    /// Earliest time marked data may be deleted.
    pub next_deletion_unix: Option<u64>,
    /// Reclaimable data (candidates, waiting and due) per account.
    pub accounts: Vec<CleanupAccount>,
    /// Why deleting the due data is refused without `--force` (mass-delete guard).
    pub guard: Option<String>,
    /// Reference sources that could not be read (everything postponed).
    pub postponed: Vec<String>,
    /// The pool's cleanup settings used for this run.
    pub settings: CleanupSettings,
    /// Extra explanations for the user.
    pub notes: Vec<String>,
}
