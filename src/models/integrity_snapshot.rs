//! Last integrity check result kept on disk ([`IntegritySnapshot`]) and the
//! per-group health ([`GroupHealth`]) shared with scrub reports. Written by
//! `maintenance::snapshot_store`, read by the GUI integrity and repair screens.
use super::ShardHealth;
use serde::{Deserialize, Serialize};

/// Current [`IntegritySnapshot`] format version.
pub(crate) const INTEGRITY_SNAPSHOT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Health of one Reed-Solomon group, from `maintenance::group_health`.
pub(crate) struct GroupHealth {
    /// Group number (0 for uncoded archives).
    pub(crate) group: u32,
    /// `healthy`, `recoverable`, `unrecoverable` or `provider-error`.
    pub(crate) status: String,
    /// Shards that are missing, have a bad size or are corrupt.
    pub(crate) bad_shards: usize,
    /// Shards that could not be checked (provider error).
    pub(crate) provider_errors: usize,
}

impl GroupHealth {
    /// Repair can rebuild this group from its healthy shards.
    pub(crate) fn is_recoverable(&self) -> bool {
        self.status == "recoverable"
    }

    /// Too few healthy shards to rebuild the group.
    pub(crate) fn is_unrecoverable(&self) -> bool {
        self.status == "unrecoverable"
    }

    /// Some shard could not be checked; the verdict is open.
    pub(crate) fn has_provider_error(&self) -> bool {
        self.status == "provider-error"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Summary of the last scrub, saved at `integrity_snapshot_path()`;
/// copies the [`ScrubReport`](super::ScrubReport) counts but keeps only the
/// non-healthy shards. All fields default so older files still load.
pub(crate) struct IntegritySnapshot {
    #[serde(default)]
    /// Format version ([`INTEGRITY_SNAPSHOT_VERSION`]; 0 in very old files).
    pub(crate) version: u32,
    #[serde(default)]
    /// Manifest address/path the check ran on.
    pub(crate) manifest_source: String,
    #[serde(default)]
    /// Checked archive.
    pub(crate) archive_id: String,
    #[serde(default)]
    /// `quick-size` or `full-blake3`.
    pub(crate) mode: String,
    #[serde(default)]
    /// When the check finished, Unix seconds.
    pub(crate) checked_unix: u64,
    #[serde(default)]
    /// Shards checked.
    pub(crate) total: usize,
    #[serde(default)]
    /// Shards that passed.
    pub(crate) healthy: usize,
    #[serde(default)]
    /// Shards not found.
    pub(crate) missing: usize,
    #[serde(default)]
    /// Shards with the wrong size.
    pub(crate) bad_size: usize,
    #[serde(default)]
    /// Shards whose BLAKE3 did not match (full mode only).
    pub(crate) corrupt: usize,
    #[serde(default)]
    /// Shards that could not be checked (provider errors).
    pub(crate) errors: usize,
    #[serde(default)]
    /// Groups with bad shards that are still recoverable.
    pub(crate) degraded_groups: usize,
    #[serde(default)]
    /// Groups that cannot be rebuilt.
    pub(crate) unrecoverable_groups: usize,
    #[serde(default)]
    /// Health per group.
    pub(crate) groups: Vec<GroupHealth>,
    #[serde(default)]
    /// Shards that are not healthy.
    pub(crate) issues: Vec<ShardHealth>,
}
