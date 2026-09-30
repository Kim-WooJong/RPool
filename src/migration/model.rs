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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MissingReason {
    /// On an account that is no longer part of the pool (and not readable).
    RemoteRemoved,
    Missing,
    BadSize,
    Corrupt,
    ProviderError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissingShard {
    pub index: u32,
    pub remote: String,
    pub reason: MissingReason,
}

/// One Reed-Solomon group that lacks shards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupLoss {
    pub group: u32,
    pub required_k: usize,
    pub available: usize,
    pub missing: Vec<MissingShard>,
}

/// One archive of the pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub archive_id: String,
    pub original_name: String,
    pub size: u64,
    /// Where the manifest was read (local path or `remote:path/manifest.json`).
    pub source: String,
    /// `manifest_fingerprint` of the source manifest when planned.
    pub fingerprint: String,
    pub action: Action,
    /// Estimated transfer for this entry.
    pub download_bytes: u64,
    pub upload_bytes: u64,
    /// Groups short of shards (for `Lost`, and for degraded `Relocate`).
    #[serde(default)]
    pub losses: Vec<GroupLoss>,
    /// Human-readable reason for `Unknown`, or notes.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Counts {
    pub unaffected: usize,
    pub relocate: usize,
    pub reencode: usize,
    pub lost: usize,
    pub unknown: usize,
}

/// A frozen migration plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub version: u32,
    pub migration_id: String,
    pub pool: String,
    pub created_unix: u64,
    /// PC name that made the plan.
    pub created_by: String,
    /// The pool policy to migrate to (the saved pool at plan time).
    pub target: PoolDefinition,
    pub entries: Vec<Entry>,
    pub counts: Counts,
    pub download_bytes: u64,
    pub upload_bytes: u64,
    /// Extra cloud space needed by the new copies (originals are kept).
    pub new_storage_bytes: u64,
    /// Estimated duration range in seconds (fast, slow), if speeds are known.
    pub estimated_seconds: Option<(f64, f64)>,
    /// Speeds used for the estimate, MiB/s.
    pub download_mib_s: Option<f64>,
    pub upload_mib_s: Option<f64>,
    /// Quota check against the new policy: Some(true) fits, Some(false) does
    /// not, None unknown.
    pub quota_ok: Option<bool>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Progress of one entry, recorded in the cloud journal. States only move
/// forward; the highest state of an entry wins when folding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RecordState {
    Claimed,
    Verified,
    Switched,
    Lost,
    Unknown,
    Orphan,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// `archive_id` of the plan entry; empty for migration-level records
    /// (for example `Abandoned`).
    pub entry: String,
    pub state: RecordState,
    pub attempt_id: String,
    pub pc_id: String,
    pub ts_unix: u64,
    /// The replacement archive (for `Verified` / `Switched`).
    #[serde(default)]
    pub new_archive_id: Option<String>,
    /// Where the replacement manifest can be read, `remote:path/manifest.json`.
    #[serde(default)]
    pub new_manifest: Option<String>,
    #[serde(default)]
    pub losses: Vec<GroupLoss>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// A file that cannot be recovered, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LostFile {
    pub archive_id: String,
    pub original_name: String,
    pub size: u64,
    pub groups: Vec<GroupLoss>,
    /// "plan" or "run".
    pub detected: String,
}

/// Summary of one migration for `status` and the GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MigrationStatus {
    pub migration_id: String,
    pub pool: String,
    pub created_unix: u64,
    pub created_by: String,
    pub counts: Counts,
    /// Entries needing work (relocate + reencode).
    pub to_move: usize,
    pub switched: usize,
    pub verified: usize,
    pub failed_unknown: usize,
    pub lost: Vec<LostFile>,
    pub abandoned: bool,
    /// Every movable entry is switched (lost ones excluded).
    pub complete: bool,
    /// PCs that recorded work, most recent first.
    pub pcs: Vec<String>,
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
