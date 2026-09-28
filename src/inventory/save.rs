use crate::config::inventory_path;
use crate::models::InventoryStore;
use crate::utils::save_json_atomic;
use anyhow::Result;
use std::path::PathBuf;

pub(crate) fn save_inventory(store: &InventoryStore) -> Result<PathBuf> {
    let path = inventory_path()?;
    save_json_atomic(&path, store)?;
    Ok(path)
}
