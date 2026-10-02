//! Building inventory entries from manifests.

use crate::manifest::manifest_remotes;
use crate::models::{InventoryEntry, Manifest};
use crate::utils::now_unix;

/// Inventory entry summarising `manifest` (name, size, coding, remotes),
/// recording where the manifest was read from and when it was indexed.
pub(crate) fn entry_from_manifest(
    manifest: &Manifest,
    source: impl Into<String>,
) -> InventoryEntry {
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
