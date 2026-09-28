use crate::inventory::{entry_from_manifest, load_inventory, save_inventory};
use crate::manifest::{load_manifest, validate_manifest};
use anyhow::Result;

pub(crate) fn add_manifest(rclone: &str, source: &str) -> Result<()> {
    let manifest = load_manifest(rclone, source)?;
    validate_manifest(&manifest)?;
    let entry = entry_from_manifest(&manifest, source.to_string());
    let mut store = load_inventory()?;
    store.entries.insert(entry.archive_id.clone(), entry);
    save_inventory(&store)?;
    Ok(())
}
