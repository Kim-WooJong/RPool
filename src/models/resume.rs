use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResumeState {
    #[serde(default)]
    pub(crate) version: u32,
    pub(crate) manifest_fingerprint: String,
    #[serde(default)]
    pub(crate) original_size: u64,
    pub(crate) completed: BTreeSet<u32>,
    #[serde(default)]
    pub(crate) updated_unix: u64,
}
