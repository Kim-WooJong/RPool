//! Effective `put` settings after merging CLI flags with a pool definition
//! ([`ResolvedPutOptions`], built by `pool::resolve`).
use super::Placement;

#[derive(Debug, Clone)]
/// Settings one `put` runs with.
pub(crate) struct ResolvedPutOptions {
    /// Target remotes, with remote roots applied.
    pub(crate) remotes: Vec<String>,
    /// Shard size, MiB.
    pub(crate) shard_mib: u64,
    /// Parallel transfers.
    pub(crate) workers: usize,
    /// Retries per shard transfer.
    pub(crate) retries: u32,
    /// Shard placement policy.
    pub(crate) placement: Placement,
    /// Data shards per group (K).
    pub(crate) data_shards: usize,
    /// Parity shards per group (M).
    pub(crate) parity_shards: usize,
    /// Pool the settings came from; `None` for an ad-hoc remote list.
    pub(crate) pool_name: Option<String>,
    /// Encrypt shards in RPool and write to the crypt base (pool option).
    pub(crate) native_crypt: bool,
}
