use super::Coding;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InventoryEntry {
    pub(crate) archive_id: String,
    pub(crate) original_name: String,
    pub(crate) original_size: u64,
    pub(crate) created_unix: u64,
    pub(crate) content_root_blake3: String,
    pub(crate) coding: Option<Coding>,
    pub(crate) remotes: Vec<String>,
    pub(crate) manifest_source: String,
    pub(crate) indexed_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InventoryStore {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) entries: BTreeMap<String, InventoryEntry>,
}

impl Default for InventoryStore {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}
