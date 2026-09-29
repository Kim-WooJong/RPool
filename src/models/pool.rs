use super::shard_size::ShardSize;
use super::Placement;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PoolDefinition {
    pub(crate) remotes: Vec<String>,
    /// Serialized as the historical integer `shard_mib` field for MiB-aligned sizes.
    #[serde(rename = "shard_mib", alias = "shard_size")]
    pub(crate) shard_size: ShardSize,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
}

impl PoolDefinition {
    /// Validated plaintext shard size in bytes.
    pub(crate) fn shard_bytes(&self) -> anyhow::Result<std::num::NonZeroU64> {
        self.shard_size.validate()
    }

    /// Validated shard size in whole MiB (for MiB-based interfaces such as `put`).
    pub(crate) fn shard_mib(&self) -> anyhow::Result<u64> {
        Ok(self.shard_bytes()?.get() / super::shard_size::MIB)
    }
}

impl Default for PoolDefinition {
    fn default() -> Self {
        Self {
            remotes: Vec::new(),
            shard_size: ShardSize::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_pool_store_json_round_trips_shard_mib() {
        for mib in [64u64, 220] {
            let legacy = format!(
                r#"{{"version":1,"pools":{{"main":{{"remotes":["a:","b:"],"shard_mib":{mib},"workers":4,"retries":3,"placement":"round-robin","data_shards":2,"parity_shards":1}}}}}}"#
            );
            let store: PoolStore = serde_json::from_str(&legacy).unwrap();
            let pool = &store.pools["main"];
            assert_eq!(pool.shard_bytes().unwrap().get(), mib * 1024 * 1024);
            assert_eq!(pool.shard_mib().unwrap(), mib);
            let value: serde_json::Value = serde_json::to_value(&store).unwrap();
            assert_eq!(value["pools"]["main"]["shard_mib"], serde_json::json!(mib));
            assert!(value["pools"]["main"].get("shard_size").is_none());
        }
        // Missing field keeps the serde(default) behaviour.
        let store: PoolStore =
            serde_json::from_str(r#"{"version":1,"pools":{"p":{"remotes":["a:"]}}}"#).unwrap();
        assert_eq!(
            store.pools["p"].shard_mib().unwrap(),
            crate::config::constants::DEFAULT_SHARD_MIB
        );
        // Out-of-range stored values stay readable but fail validation.
        let store: PoolStore = serde_json::from_str(
            r#"{"version":1,"pools":{"p":{"remotes":["a:"],"shard_mib":1048576}}}"#,
        )
        .unwrap();
        assert!(store.pools["p"].shard_bytes().is_err());
    }
}
