//! Adding or updating one archive in the local inventory.

use crate::inventory::{entry_from_manifest, load_inventory, save_inventory};
use crate::manifest::{load_manifest, validate_manifest};
use anyhow::Result;

/// Loads and validates the manifest at `source` and stores its entry in the
/// inventory under a file lock, so concurrent processes do not lose updates.
/// Called after `put`, manifest recovery, provider drain and `inventory add`.
pub(crate) fn add_manifest(rclone: &str, source: &str) -> Result<()> {
    let manifest = load_manifest(rclone, source)?;
    validate_manifest(&manifest)?;
    let entry = entry_from_manifest(&manifest, source.to_string());
    // Serialize read-modify-write across independent reprocess plans. The OS
    // releases this lock if the process is cancelled; do not unlink the inode.
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
    store.entries.insert(entry.archive_id.clone(), entry);
    save_inventory(&store)?;
    Ok(())
}
