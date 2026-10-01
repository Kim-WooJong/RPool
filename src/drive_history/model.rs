//! JSON contract of `rpool drive trash|versions|rollback --json` (CLI ↔ GUI).
#![allow(dead_code)] // Contract stub: remove once CLI and GUI use it.
use serde::{Deserialize, Serialize};

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
    pub trash_days: u32,
    pub keep_versions: u32,
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
    pub is_dir: bool,
    /// Bytes of the last version (folders: total of contained files).
    pub size: u64,
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
pub(crate) enum VersionKind {
    Created,
    Modified,
    Deleted,
    Restored,
}

/// One revision of a file, newest first in listings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VersionEntry {
    /// Revision/event id (input of `versions restore`).
    pub id: String,
    pub path: String,
    pub kind: VersionKind,
    pub size: u64,
    pub time_unix: Option<u64>,
    pub author: Option<String>,
    /// The version the drive shows now.
    pub current: bool,
    /// Data still available (not expired/purged), so it can be restored.
    pub restorable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeAction {
    /// File existed at the target time and differs now: its old version comes back.
    Revert,
    /// File was deleted after the target time: it comes back.
    Undelete,
    /// File was created after the target time: it moves to the trash.
    Remove,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RollbackChange {
    pub path: String,
    pub action: ChangeAction,
    /// Revision restored (Revert/Undelete) or removed (Remove).
    pub revision: String,
    pub size: u64,
}

/// `rpool drive rollback` result (preview unless `applied`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RollbackPlan {
    pub version: u32,
    pub pool: String,
    /// Folder rolled back (`/` = whole drive).
    pub scope: String,
    pub at_unix: u64,
    pub changes: Vec<RollbackChange>,
    /// Paths that cannot be rolled back (data expired/purged), with reason.
    pub skipped: Vec<(String, String)>,
    pub applied: bool,
}
