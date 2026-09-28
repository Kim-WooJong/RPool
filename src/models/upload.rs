use super::{Coding, Placement, ShardKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UploadPlan {
    pub(crate) version: u32,
    pub(crate) archive_id: String,
    pub(crate) source_size: u64,
    pub(crate) shard_size: u64,
    pub(crate) remotes: Vec<String>,
    pub(crate) placement: Placement,
    #[serde(default)]
    pub(crate) coding: Option<Coding>,
    pub(crate) shards: Vec<PlanShard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanShard {
    pub(crate) index: u32,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) remote: String,
    pub(crate) object: String,
    #[serde(default)]
    pub(crate) kind: ShardKind,
    #[serde(default)]
    pub(crate) group: u32,
    #[serde(default)]
    pub(crate) slot: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct PhysicalSpec {
    pub(crate) group: u32,
    pub(crate) size: u64,
}

#[derive(Debug)]
pub(crate) struct GeneratedParity {
    pub(crate) plan: PlanShard,
    pub(crate) path: PathBuf,
    pub(crate) blake3: String,
}
