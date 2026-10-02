//! Built-in defaults and fixed limits shared by the CLI argument defaults,
//! pool definitions, upload/restore and Reed-Solomon coding.
/// Default plaintext data-shard size in MiB for new pools and `put`.
pub(crate) const DEFAULT_SHARD_MIB: u64 = 64;
/// Default number of concurrent shard transfers / checks.
pub(crate) const DEFAULT_WORKERS: usize = 8;
/// Default whole-shard attempts before a transfer fails.
pub(crate) const DEFAULT_RETRIES: u32 = 3;
/// Default Reed-Solomon data shards per coding group (K).
pub(crate) const DEFAULT_DATA_SHARDS: usize = 8;
/// Default parity shards per group (M); 0 means no erasure coding.
pub(crate) const DEFAULT_PARITY_SHARDS: usize = 0;
/// Buffer size for local file copies and hashing (4 MiB).
pub(crate) const IO_BUFFER: usize = 4 * 1024 * 1024;
/// Bytes encoded/reconstructed per step within a shard for new archives
/// (stored in the manifest's `Coding::stripe_size`).
pub(crate) const EC_STRIPE_SIZE: usize = 4 * 1024 * 1024;
/// Algorithm name recorded in manifests for RPool's Reed-Solomon over GF(2^8).
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
