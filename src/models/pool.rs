use serde::{Deserialize, Serialize};
use super::Placement;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PoolDefinition {
    pub(crate) remotes: Vec<String>,
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
}

impl Default for PoolDefinition {
    fn default() -> Self {
        Self {
            remotes: Vec::new(),
            shard_mib: crate::config::constants::DEFAULT_SHARD_MIB,
            workers: crate::config::constants::DEFAULT_WORKERS,
            retries: crate::config::constants::DEFAULT_RETRIES,
            placement: Placement::RoundRobin,
            data_shards: crate::config::constants::DEFAULT_DATA_SHARDS,
            parity_shards: crate::config::constants::DEFAULT_PARITY_SHARDS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PoolStore {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) pools: std::collections::BTreeMap<String, PoolDefinition>,
}

impl Default for PoolStore {
    fn default() -> Self {
        Self {
            version: 1,
            pools: std::collections::BTreeMap::new(),
        }
    }
}
