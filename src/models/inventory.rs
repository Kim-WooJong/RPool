//! Local archive index ([`InventoryStore`]): one [`InventoryEntry`] per known
//! manifest, maintained by the `inventory` module and used by listing,
//! migration and cleanup to find archives without scanning the cloud.
use super::Coding;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Summary of one archive's manifest, built by `inventory::entry`.
pub(crate) struct InventoryEntry {
    /// Archive id (also the map key in [`InventoryStore::entries`]).
    pub(crate) archive_id: String,
    /// File name of the original file.
    pub(crate) original_name: String,
    /// Original file size in bytes.
    pub(crate) original_size: u64,
    /// When the archive was created, Unix seconds (from the manifest).
    pub(crate) created_unix: u64,
    /// Manifest content root (BLAKE3 hex), identifying the archive content.
    pub(crate) content_root_blake3: String,
    /// Erasure coding; `None` for uncoded archives.
    pub(crate) coding: Option<Coding>,
    /// Remotes holding the archive's shards.
    pub(crate) remotes: Vec<String>,
    /// Where the manifest was read from (local path or remote address).
    pub(crate) manifest_source: String,
    /// When this entry was indexed, Unix seconds.
    pub(crate) indexed_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// The inventory file: archive id -> entry.
pub(crate) struct InventoryStore {
    /// File format version (1).
    pub(crate) version: u32,
    #[serde(default)]
    /// Entries keyed by archive id.
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
