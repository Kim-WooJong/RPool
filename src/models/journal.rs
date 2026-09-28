use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use super::Shard;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UploadJournal {
    pub(crate) version: u32,
    pub(crate) plan_fingerprint: String,
    #[serde(default)]
    pub(crate) completed: BTreeMap<u32, Shard>,
    #[serde(default)]
    pub(crate) updated_unix: u64,
}
