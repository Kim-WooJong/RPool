use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Coding {
    pub(crate) algorithm: String,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) stripe_size: usize,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ShardKind {
    Data,
    Parity,
}

impl Default for ShardKind {
    fn default() -> Self {
        Self::Data
    }
}
