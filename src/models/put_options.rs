use super::Placement;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPutOptions {
    pub(crate) remotes: Vec<String>,
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) placement: Placement,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) pool_name: Option<String>,
    pub(crate) native_crypt: bool,
}
