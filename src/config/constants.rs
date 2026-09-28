pub(crate) const DEFAULT_SHARD_MIB: u64 = 220;
pub(crate) const DEFAULT_WORKERS: usize = 8;
pub(crate) const DEFAULT_RETRIES: u32 = 3;
pub(crate) const DEFAULT_DATA_SHARDS: usize = 8;
pub(crate) const DEFAULT_PARITY_SHARDS: usize = 0;
pub(crate) const IO_BUFFER: usize = 4 * 1024 * 1024;
pub(crate) const EC_STRIPE_SIZE: usize = 4 * 1024 * 1024;
pub(crate) const RS_ALGORITHM: &str = "reed-solomon-gf256";
