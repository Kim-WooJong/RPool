//! Scrub results: per-shard health ([`ShardHealth`]) and the archive report
//! ([`ScrubReport`]) produced by `maintenance::scan` and shown by `scrub`,
//! `repair` and the GUI integrity screen.
use super::{Probe, Shard, ShardKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Probe outcome of one shard, with its manifest location.
pub(crate) struct ShardHealth {
    /// Shard index in the manifest.
    pub(crate) index: u32,
    /// Data or parity.
    pub(crate) kind: ShardKind,
    /// Reed-Solomon group.
    pub(crate) group: u32,
    /// Position within the group (data first, then parity).
    pub(crate) slot: u16,
    /// Remote holding the shard.
    pub(crate) remote: String,
    /// Object address of the shard.
    pub(crate) object: String,
    /// Size recorded in the manifest, bytes.
    pub(crate) expected_size: u64,
    /// `healthy`, `missing`, `bad-size`, `corrupt` or `error`.
    pub(crate) status: String,
    /// Found/expected values or the provider error message.
    pub(crate) detail: Option<String>,
}

impl ShardHealth {
    /// Converts a probe result into a health row; called by `maintenance::scan`.
    pub(crate) fn from_probe(shard: &Shard, probe: &Probe) -> Self {
        let (status, detail) = match probe {
            Probe::Ok => ("healthy".to_string(), None),
            Probe::Missing => ("missing".to_string(), None),
            Probe::BadSize { found, expected } => (
                "bad-size".to_string(),
                Some(format!("found={found} expected={expected}")),
            ),
            Probe::Corrupt { found, expected } => (
                "corrupt".to_string(),
                Some(format!("found={found} expected={expected}")),
            ),
            Probe::Error(message) => ("error".to_string(), Some(message.clone())),
        };
        Self {
            index: shard.index,
            kind: shard.kind,
            group: shard.group,
            slot: shard.slot,
            remote: shard.remote.clone(),
            object: shard.object.clone(),
            expected_size: shard.size,
            status,
            detail,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Result of scrubbing one archive.
pub(crate) struct ScrubReport {
    /// Scrubbed archive.
    pub(crate) archive_id: String,
    /// `quick-size` (sizes only) or `full-blake3` (content hashes).
    pub(crate) mode: String,
    /// Shards checked.
    pub(crate) total: usize,
    /// Shards that passed.
    pub(crate) healthy: usize,
    /// Shards not found.
    pub(crate) missing: usize,
    /// Shards with the wrong size.
    pub(crate) bad_size: usize,
    /// Shards with a wrong BLAKE3 hash.
    pub(crate) corrupt: usize,
    /// Shards that could not be checked (provider errors).
    pub(crate) errors: usize,
    /// Groups with bad shards that are still recoverable.
    pub(crate) degraded_groups: usize,
    /// Groups that cannot be rebuilt.
    pub(crate) unrecoverable_groups: usize,
    #[serde(default)]
    /// Health per Reed-Solomon group.
    pub(crate) groups: Vec<super::GroupHealth>,
    /// Health of every shard, in manifest order.
    pub(crate) shards: Vec<ShardHealth>,
}
