//! Shared data model of a migration (CONTRACT).
use crate::prelude::*;

/// What an archive needs under the new policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Action {
    /// Already valid under the new policy.
    Unaffected,
    /// Same coding, but shards sit on remotes no longer in the pool (or the
    /// placement is invalid): rebuild onto the new remotes.
    Relocate,
    /// Coding changed (K, M, shard size or native crypt): re-encode.
    Reencode,
    /// Some group has fewer than K readable shards: cannot be recovered.
    Lost,
    /// A provider error prevented the check; retry later. Never treated as lost.
    Unknown,
}

/// Why a shard counts as missing in a [`GroupLoss`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MissingReason {
    /// On an account that is no longer part of the pool (and not readable).
    RemoteRemoved,
    /// The object does not exist.
    Missing,
    /// The object has the wrong size.
    BadSize,
    /// The object's content hash does not match (full probe).
    Corrupt,
    /// The provider could not be queried; state unknown.
    ProviderError,
}

/// A shard of a group that is not readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissingShard {
    /// Shard index in the manifest.
    pub index: u32,
    /// Remote the shard was stored on.
    pub remote: String,
    /// Why it is not usable.
    pub reason: MissingReason,
}

/// One Reed-Solomon group that lacks shards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupLoss {
    /// Coding group number.
    pub group: u32,
    /// Shards needed to reconstruct the group (K, or all shards without coding).
    pub required_k: usize,
    /// Readable shards plus virtual zero data shards of a short last group.
    pub available: usize,
    /// The unreadable shards.
    pub missing: Vec<MissingShard>,
}

/// One archive of the pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Archive id.
    pub archive_id: String,
    /// Original file name.
    pub original_name: String,
    /// Original size in bytes.
    pub size: u64,
    /// Where the manifest was read (local path or `remote:path/manifest.json`).
    pub source: String,
    /// `manifest_fingerprint` of the source manifest when planned.
    pub fingerprint: String,
    /// What the run does with this archive.
    pub action: Action,
    /// Estimated transfer for this entry.
    pub download_bytes: u64,
    /// Estimated upload bytes for this entry.
    pub upload_bytes: u64,
    /// Groups short of shards (for `Lost`, and for degraded `Relocate`).
    #[serde(default)]
    pub losses: Vec<GroupLoss>,
    /// Human-readable reason for `Unknown`, or notes.
    #[serde(default)]
    pub detail: Option<String>,
    /// Rebalance destinations (shard index -> remote) the run must use.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub moves: BTreeMap<u32, String>,
}

/// Number of plan entries per [`Action`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Counts {
    /// Archives already valid under the new policy.
    pub unaffected: usize,
    /// Archives to relocate.
    pub relocate: usize,
    /// Archives to re-encode.
    pub reencode: usize,
    /// Unrecoverable archives.
    pub lost: usize,
    /// Archives whose check hit a provider error.
    pub unknown: usize,
}

/// A frozen migration plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Plan {
    /// Plan format version.
    pub version: u32,
    /// Unique id of this migration.
    pub migration_id: String,
    /// Pool being migrated.
    pub pool: String,
    /// When the plan was made, Unix seconds.
    pub created_unix: u64,
    /// PC name that made the plan.
    pub created_by: String,
    /// The pool policy to migrate to (the saved pool at plan time).
    pub target: PoolDefinition,
    /// One entry per archive of the pool.
    pub entries: Vec<Entry>,
    /// Entry counts per action.
    pub counts: Counts,
    /// Total estimated download bytes.
    pub download_bytes: u64,
    /// Total estimated upload bytes.
    pub upload_bytes: u64,
    /// Extra cloud space needed by the new copies (originals are kept).
    pub new_storage_bytes: u64,
    /// Estimated duration range in seconds (fast, slow), if speeds are known.
    pub estimated_seconds: Option<(f64, f64)>,
    /// Speeds used for the estimate, MiB/s.
    pub download_mib_s: Option<f64>,
    /// Upload speed used for the estimate, MiB/s.
    pub upload_mib_s: Option<f64>,
    /// Quota check against the new policy: Some(true) fits, Some(false) does
    /// not, None unknown.
    pub quota_ok: Option<bool>,
    /// Human-readable planning notes and warnings.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Progress of one entry, recorded in the cloud journal. States only move
/// forward; the highest state of an entry wins when folding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RecordState {
    /// A PC started building a replacement.
    Claimed,
    /// The replacement was built and fully verified.
    Verified,
    /// The replacement is in use (inventory switched).
    Switched,
    /// The entry cannot be recovered.
    Lost,
    /// The attempt ended with a provider error; retry later.
    Unknown,
    /// An interrupted attempt's partial replacement, left as an orphan.
    Orphan,
    /// The whole migration was abandoned (migration-level record).
    Abandoned,
}

/// One append-only journal record about an entry or the migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// `archive_id` of the plan entry; empty for migration-level records
    /// (for example `Abandoned`).
    pub entry: String,
    /// Progress state this record reports.
    pub state: RecordState,
    /// Id of the attempt (one claim/build cycle).
    pub attempt_id: String,
    /// PC that wrote the record.
    pub pc_id: String,
    /// When the record was written, Unix seconds.
    pub ts_unix: u64,
    /// The replacement archive (for `Verified` / `Switched`).
    #[serde(default)]
    pub new_archive_id: Option<String>,
    /// Where the replacement manifest can be read, `remote:path/manifest.json`.
    #[serde(default)]
    pub new_manifest: Option<String>,
    /// Group losses (for `Lost`).
    #[serde(default)]
    pub losses: Vec<GroupLoss>,
    /// Error text or notes.
    #[serde(default)]
    pub detail: Option<String>,
}

/// A file that cannot be recovered, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LostFile {
    /// Archive id (for drive files: the payload archive when planned).
    pub archive_id: String,
    /// Original name (for drive files: the drive path).
    pub original_name: String,
    /// Size in bytes.
    pub size: u64,
    /// The groups that cannot be reconstructed.
    pub groups: Vec<GroupLoss>,
    /// "plan" or "run".
    pub detected: String,
}

/// Summary of one migration for `status` and the GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MigrationStatus {
    /// Migration id.
    pub migration_id: String,
    /// Pool name.
    pub pool: String,
    /// When the plan was made, Unix seconds.
    pub created_unix: u64,
    /// PC that made the plan.
    pub created_by: String,
    /// Plan entry counts per action.
    pub counts: Counts,
    /// Entries needing work (relocate + reencode).
    pub to_move: usize,
    /// Entries switched to their replacement.
    pub switched: usize,
    /// Entries verified but not yet switched.
    pub verified: usize,
    /// Entries whose last attempt ended unknown.
    pub failed_unknown: usize,
    /// Unrecoverable files (from plan and run).
    pub lost: Vec<LostFile>,
    /// The migration was abandoned.
    pub abandoned: bool,
    /// Every movable entry is switched (lost ones excluded).
    pub complete: bool,
    /// PCs that recorded work, most recent first.
    pub pcs: Vec<String>,
    /// Time of the latest record, Unix seconds.
    pub last_activity_unix: u64,
}

/// Folds journal records into the winning record per entry: the highest state
/// wins; among equal states the smallest `(ts_unix, attempt_id)`.
pub(crate) fn fold(records: &[Record]) -> BTreeMap<String, Record> {
    let mut out: BTreeMap<String, Record> = BTreeMap::new();
    for record in records {
        match out.get(&record.entry) {
            Some(current)
                if (
                    current.state,
                    std::cmp::Reverse((current.ts_unix, &current.attempt_id)),
                ) >= (
                    record.state,
                    std::cmp::Reverse((record.ts_unix, &record.attempt_id)),
                ) => {}
            _ => {
                out.insert(record.entry.clone(), record.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(entry: &str, state: RecordState, ts: u64, attempt: &str) -> Record {
        Record {
            entry: entry.into(),
            state,
            attempt_id: attempt.into(),
            pc_id: "pc".into(),
            ts_unix: ts,
            new_archive_id: None,
            new_manifest: None,
            losses: vec![],
            detail: None,
        }
    }

    #[test]
    fn highest_state_wins_then_earliest_attempt() {
        let folded = fold(&[
            record("a", RecordState::Claimed, 1, "x"),
            record("a", RecordState::Verified, 5, "y"),
            record("a", RecordState::Verified, 3, "z"),
            record("b", RecordState::Switched, 9, "q"),
            record("b", RecordState::Claimed, 10, "r"),
        ]);
        assert_eq!(folded["a"].attempt_id, "z");
        assert_eq!(folded["b"].state, RecordState::Switched);
    }
}
