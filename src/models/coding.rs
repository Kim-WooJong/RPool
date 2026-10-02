//! Erasure-coding parameters of an archive ([`Coding`]) and the shard kind
//! ([`ShardKind`]); stored in manifests, inventory entries and upload plans.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Reed-Solomon parameters of a coded archive (`Manifest::coding`); `None`
/// there means an uncoded archive. Checked by `manifest::validate`.
pub(crate) struct Coding {
    /// Coding algorithm name; only `RS_ALGORITHM` (`reed-solomon-gf256`) is valid.
    pub(crate) algorithm: String,
    /// Data shards per group (K).
    pub(crate) data_shards: usize,
    /// Parity shards per group (M).
    pub(crate) parity_shards: usize,
    /// Bytes encoded/reconstructed per step within a shard (> 0); new archives
    /// use `EC_STRIPE_SIZE` (4 MiB).
    pub(crate) stripe_size: usize,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
/// Role of a shard in its group.
pub(crate) enum ShardKind {
    #[default]
    /// Holds a slice of the original file (default for v1 manifests).
    Data,
    /// Reed-Solomon parity computed from the group's data shards.
    Parity,
}
