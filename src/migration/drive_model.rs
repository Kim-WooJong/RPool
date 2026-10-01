//! Data model of the drive part of a migration (phase 3, WP7).
//!
//! The pool's virtual drive (v6 pool-sync events) is
//! migrated file by file like archives: each file's payload manifest is
//! relocated or re-encoded into a NEW archive, recorded in the same cloud
//! journal (records keyed by [`entry_key`]). Nothing is switched in place:
//! the drive changes only when the migrated files are **adopted** into a new
//! metadata epoch (`<root>/epochs/<epoch>/`, as "Apply pool changes" uses),
//! whose records reference the new archives. Side documents of the journal:
//!
//! - `drive-plan.json`: the frozen drive plan (no manifests; the run reads
//!   them again from the cloud);
//! - `drive-freeze.json`: written when the drive part starts; PCs still on
//!   the source generation stop publishing (their writes stay local);
//! - `drive-adoption.json`: the commit point; every PC then opens the drive
//!   on the new epoch, and workspaces on the source generation are refused.
use super::model::{Action, Counts, GroupLoss, LostFile};
use crate::prelude::*;

pub(crate) const DRIVE_PLAN: &str = "drive-plan.json";
pub(crate) const DRIVE_FREEZE: &str = "drive-freeze.json";
pub(crate) const DRIVE_ADOPTION: &str = "drive-adoption.json";

/// One metadata generation of the drive: where its records live.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct GenerationRef {
    /// `None`: the original (pre-transition) location.
    pub epoch: Option<String>,
}

impl GenerationRef {
    pub(crate) fn label(&self) -> String {
        match &self.epoch {
            None => "v6 original".into(),
            Some(epoch) => format!("v6 epoch {}", &epoch[..epoch.len().min(12)]),
        }
    }
}

/// A drive file as its generation's records publish it.
#[derive(Debug, Clone)]
pub(crate) struct DriveFile {
    pub path: String,
    /// Event id of the visible revision.
    pub revision: String,
    /// Plaintext blake3 of the whole file.
    pub hash: String,
    pub size: u64,
    pub manifest: Manifest,
}

/// The drive as read from the cloud.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceView {
    pub files: Vec<DriveFile>,
}

/// One drive file of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DriveEntry {
    /// Journal record key, [`entry_key`] of path and revision.
    pub key: String,
    pub path: String,
    pub revision: String,
    pub hash: String,
    pub size: u64,
    /// Archive id of the payload when planned (`virtual-*`).
    pub source_archive_id: String,
    pub fingerprint: String,
    pub action: Action,
    pub download_bytes: u64,
    pub upload_bytes: u64,
    #[serde(default)]
    pub losses: Vec<GroupLoss>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// The frozen drive part of a migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DrivePlan {
    pub version: u32,
    pub migration_id: String,
    pub pool: String,
    pub source: GenerationRef,
    /// The epoch the drive is adopted into ([`epoch_for`]).
    pub epoch: String,
    pub entries: Vec<DriveEntry>,
    pub counts: Counts,
    pub download_bytes: u64,
    pub upload_bytes: u64,
    pub new_storage_bytes: u64,
    pub estimated_seconds: Option<(f64, f64)>,
    /// Quota check of archives and drive together (None: unknown).
    pub quota_ok: Option<bool>,
    /// The new generation stays within the fresh-bootstrap budget
    /// (10,000 records / 64 MiB per kind).
    pub bootstrap_ok: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The drive part started: PCs on `source` stop publishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DriveFreeze {
    pub version: u32,
    pub migration_id: String,
    pub source: GenerationRef,
    pub epoch: String,
    pub pc_id: String,
    pub ts_unix: u64,
}

/// The commit point: the drive lives on `epoch` from now on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DriveAdoption {
    pub version: u32,
    pub migration_id: String,
    pub source: GenerationRef,
    pub epoch: String,
    /// Files published in the new generation.
    pub files: usize,
    /// Unrecoverable paths left out (`--accept-lost`).
    #[serde(default)]
    pub dropped: Vec<String>,
    pub pc_id: String,
    pub ts_unix: u64,
}

impl DriveAdoption {
    pub(crate) fn generation(&self) -> GenerationRef {
        GenerationRef {
            epoch: Some(self.epoch.clone()),
        }
    }
    /// Copies written by different PCs differ only in who and when.
    pub(crate) fn same_meaning(&self, other: &Self) -> bool {
        self.migration_id == other.migration_id
            && self.source == other.source
            && self.epoch == other.epoch
    }
}

/// Drive progress of one migration, for `status` and the GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DriveStatus {
    pub migration_id: String,
    pub source: GenerationRef,
    pub epoch: String,
    pub counts: Counts,
    /// Planned entries needing a new archive (relocate + reencode).
    pub to_move: usize,
    pub switched: usize,
    pub verified: usize,
    pub failed_unknown: usize,
    pub lost: Vec<LostFile>,
    pub frozen: bool,
    pub adopted: Option<DriveAdoption>,
    pub bootstrap_ok: bool,
    /// Every movable planned entry has its new archive; adoption may still
    /// catch up files changed since planning.
    pub ready: bool,
}

/// Journal record key of a drive file revision.
pub(crate) fn entry_key(path: &str, revision: &str) -> String {
    let hash = blake3::hash(
        &serde_json::to_vec(&("rpool-migration-drive-entry-v1", path, revision))
            .unwrap_or_default(),
    );
    format!("drive-{}", &hash.to_hex()[..40])
}

pub(crate) fn is_drive_key(entry: &str) -> bool {
    entry.starts_with("drive-")
}

/// The epoch a migration adopts its drive into. Deterministic, so every PC
/// adopting the same migration writes the same records.
pub(crate) fn epoch_for(migration_id: &str) -> String {
    blake3::hash(
        &serde_json::to_vec(&("rpool-migration-drive-epoch-v1", migration_id)).unwrap_or_default(),
    )
    .to_hex()
    .to_string()
}

/// Fresh-bootstrap budget of one generation (`pool_sync::collect`).
pub(crate) const BOOTSTRAP_RECORDS: usize = 10_000;
pub(crate) const BOOTSTRAP_BYTES: u64 = 64 * 1024 * 1024;

/// Conservative size of the record(s) one adopted file needs.
pub(crate) fn record_bytes(path: &str, manifest: &Manifest) -> u64 {
    serde_json::to_vec(manifest).map_or(0, |b| b.len() as u64) + path.len() as u64 + 1024
}
