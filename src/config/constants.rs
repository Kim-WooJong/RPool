pub(crate) const DEFAULT_SHARD_MIB: u64 = 64;
pub(crate) const DEFAULT_WORKERS: usize = 8;
pub(crate) const DEFAULT_RETRIES: u32 = 3;
pub(crate) const DEFAULT_DATA_SHARDS: usize = 8;
pub(crate) const DEFAULT_PARITY_SHARDS: usize = 0;
pub(crate) const IO_BUFFER: usize = 4 * 1024 * 1024;
pub(crate) const EC_STRIPE_SIZE: usize = 4 * 1024 * 1024;
pub(crate) const RS_ALGORITHM: &str = "reed-solomon-gf256";
/// Upper bound for a plaintext shard size, in MiB (4 GiB).
///
/// Justification: download staging is bounded by roughly
/// `(workers + 2 * M) * shard_size` (see README "two active groups"), so with the
/// default 8 workers and M = 2 a 4 GiB shard already implies ~48 GiB of local
/// staging. 4 GiB plus rclone crypt framing (~0.025%) also stays below common
/// single-request object limits such as S3's 5 GiB single PUT. Anything larger
/// is almost certainly a unit mistake (e.g. bytes entered as MiB).
pub(crate) const MAX_SHARD_MIB: u64 = 4096;
