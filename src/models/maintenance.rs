use serde::{Deserialize, Serialize};
use super::{Probe, Shard, ShardKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShardHealth {
    pub(crate) index: u32,
    pub(crate) kind: ShardKind,
    pub(crate) group: u32,
    pub(crate) slot: u16,
    pub(crate) remote: String,
    pub(crate) object: String,
    pub(crate) expected_size: u64,
    pub(crate) status: String,
    pub(crate) detail: Option<String>,
}

impl ShardHealth {
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
pub(crate) struct ScrubReport {
    pub(crate) archive_id: String,
    pub(crate) mode: String,
    pub(crate) total: usize,
    pub(crate) healthy: usize,
    pub(crate) missing: usize,
    pub(crate) bad_size: usize,
    pub(crate) corrupt: usize,
    pub(crate) errors: usize,
    pub(crate) degraded_groups: usize,
    pub(crate) unrecoverable_groups: usize,
    #[serde(default)]
    pub(crate) groups: Vec<super::GroupHealth>,
    pub(crate) shards: Vec<ShardHealth>,
}
