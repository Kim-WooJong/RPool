use crate::inventory::{load_inventory, save_inventory};
use anyhow::Result;

/// Removes `archive_id` from the local inventory (the migration cleanup
/// deletes its objects). `Ok(false)` when it was not indexed.
pub(crate) fn remove_entry(archive_id: &str) -> Result<bool> {
    // Same read-modify-write lock as `add_manifest`.
    let path = crate::config::inventory_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut store = load_inventory()?;
    if store.entries.remove(archive_id).is_none() {
        return Ok(false);
    }
    save_inventory(&store)?;
    Ok(true)
}
