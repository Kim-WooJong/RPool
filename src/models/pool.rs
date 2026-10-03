//! Pool configuration: one [`PoolDefinition`] per named pool, saved together
//! in the [`PoolStore`] file (`pool::load_pool_store`).
use super::shard_size::ShardSize;
use super::Placement;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
/// Settings of one pool: remotes, shard size, coding and placement. Used by
/// `put`, the drive, migration targets and portable export.
pub(crate) struct PoolDefinition {
    /// Pool remotes (`name:` or `name:path`), resolved with remote roots on use.
    pub(crate) remotes: Vec<String>,
    /// Serialized as the historical integer `shard_mib` field for MiB-aligned sizes.
    #[serde(rename = "shard_mib", alias = "shard_size")]
    pub(crate) shard_size: ShardSize,
    /// Parallel transfers.
    pub(crate) workers: usize,
    /// Retries per shard transfer.
    pub(crate) retries: u32,
    /// How shards are spread over the remotes.
    pub(crate) placement: Placement,
    /// Data shards per group (K).
    pub(crate) data_shards: usize,
    /// Parity shards per group (M); 0 stores uncoded archives.
    pub(crate) parity_shards: usize,
    /// Optional provider per-object limit in bytes, checked against the encrypted
    /// (rclone crypt) shard size. Omitted from JSON when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_object_bytes: Option<u64>,
    /// Opt-in: `put` and reprocess encrypt shards in RPool (rclone-crypt compatible)
    /// and write them to the crypt remote's base. Omitted from JSON when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) native_crypt: bool,
    /// Opt-in: the drive uploads small files (≤ 1 MiB) together as one
    /// archive per batch (`mount::virtual_drive::pack`). Older RPool cannot
    /// read such files and stops syncing the pool, so every PC must run a
    /// version that supports packs. Omitted from JSON when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) small_file_packing: bool,
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
            max_object_bytes: None,
            native_crypt: false,
            small_file_packing: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// The pool configuration file: pool name -> definition, plus per-pool drive
/// retention and cleanup settings.
pub(crate) struct PoolStore {
    /// File format version (1).
    pub(crate) version: u32,
    #[serde(default)]
    /// Pool definitions by pool name.
    pub(crate) pools: std::collections::BTreeMap<String, PoolDefinition>,
    /// Drive trash/version retention per pool name (`rpool drive retention`).
    /// Kept beside the definitions so it travels with portable export/import;
    /// a missing entry means the defaults. Older RPool ignores the field.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) retention:
        std::collections::BTreeMap<String, crate::drive_history::model::Retention>,
    /// Automatic drive cleanup per pool name (`rpool drive retention set
    /// --auto-cleanup/--cleanup-grace-days`); a missing entry means the
    /// defaults (on, 7 days). Older RPool ignores the field.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) drive_cleanup:
        std::collections::BTreeMap<String, crate::drive_history::model::CleanupSettings>,
}

impl Default for PoolStore {
    fn default() -> Self {
        Self {
            version: 1,
            pools: std::collections::BTreeMap::new(),
            retention: std::collections::BTreeMap::new(),
            drive_cleanup: std::collections::BTreeMap::new(),
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
