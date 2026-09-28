use crate::manifest::manifest_remotes;
use crate::models::{InventoryEntry, Manifest};
use crate::utils::now_unix;

pub(crate) fn entry_from_manifest(manifest: &Manifest, source: impl Into<String>) -> InventoryEntry {
    InventoryEntry {
        archive_id: manifest.archive_id.clone(),
        original_name: manifest.original_name.clone(),
        original_size: manifest.original_size,
        created_unix: manifest.created_unix,
        content_root_blake3: manifest.content_root_blake3.clone(),
        coding: manifest.coding.clone(),
        remotes: manifest_remotes(manifest),
        manifest_source: source.into(),
        indexed_unix: now_unix(),
    }
}
