//! Searching the local inventory.

use crate::models::{InventoryEntry, InventoryStore};
use crate::utils::wildcard_match;

/// Entries whose name, archive id or manifest source match `pattern`; a
/// pattern without `*`/`?` matches as a substring. Used by `inventory find`.
pub(crate) fn find_entries<'a>(
    store: &'a InventoryStore,
    pattern: &str,
) -> Vec<&'a InventoryEntry> {
    let normalized = if pattern.contains('*') || pattern.contains('?') {
        pattern.to_string()
    } else {
        format!("*{pattern}*")
    };

    store
        .entries
        .values()
        .filter(|entry| {
            wildcard_match(&normalized, &entry.original_name)
                || wildcard_match(&normalized, &entry.archive_id)
                || wildcard_match(&normalized, &entry.manifest_source)
        })
        .collect()
}
