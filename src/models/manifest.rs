use super::{Coding, ShardKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) version: u32,
    pub(crate) archive_id: String,
    pub(crate) original_name: String,
    pub(crate) original_size: u64,
    pub(crate) shard_size: u64,
    pub(crate) created_unix: u64,
    pub(crate) content_root_blake3: String,
    #[serde(default)]
    pub(crate) coding: Option<Coding>,
    pub(crate) shards: Vec<Shard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Shard {
    pub(crate) index: u32,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) remote: String,
    pub(crate) object: String,
    pub(crate) blake3: String,
    #[serde(default)]
    pub(crate) kind: ShardKind,
    #[serde(default)]
    pub(crate) group: u32,
    #[serde(default)]
    pub(crate) slot: u16,
}
