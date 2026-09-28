use serde::{Deserialize, Serialize};
use super::ShardHealth;

pub(crate) const INTEGRITY_SNAPSHOT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct GroupHealth {
    pub(crate) group: u32,
    pub(crate) status: String,
    pub(crate) bad_shards: usize,
    pub(crate) provider_errors: usize,
}

impl GroupHealth {
    pub(crate) fn is_recoverable(&self) -> bool {
        self.status == "recoverable"
    }

    pub(crate) fn is_unrecoverable(&self) -> bool {
        self.status == "unrecoverable"
    }

    pub(crate) fn has_provider_error(&self) -> bool {
        self.status == "provider-error"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct IntegritySnapshot {
    #[serde(default)]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) manifest_source: String,
    #[serde(default)]
    pub(crate) archive_id: String,
    #[serde(default)]
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) checked_unix: u64,
    #[serde(default)]
    pub(crate) total: usize,
    #[serde(default)]
    pub(crate) healthy: usize,
    #[serde(default)]
    pub(crate) missing: usize,
    #[serde(default)]
    pub(crate) bad_size: usize,
    #[serde(default)]
    pub(crate) corrupt: usize,
    #[serde(default)]
    pub(crate) errors: usize,
    #[serde(default)]
    pub(crate) degraded_groups: usize,
    #[serde(default)]
    pub(crate) unrecoverable_groups: usize,
    #[serde(default)]
    pub(crate) groups: Vec<GroupHealth>,
    #[serde(default)]
    pub(crate) issues: Vec<ShardHealth>,
}
