//! Reed-Solomon erasure coding over GF(256): parity generation for uploads,
//! reconstruction of missing data shards for downloads, and count validation.
/// Parity shard generation for one coding group.
mod encode;
/// Recovery of missing data shards from parity.
mod reconstruct;
/// Data/parity count checks.
mod validate;

pub(crate) use encode::generate_parity_group;
pub(crate) use reconstruct::reconstruct_group;
pub(crate) use validate::validate_rs_counts;
