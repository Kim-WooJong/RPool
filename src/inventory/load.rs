//! Reading the local inventory index.

use crate::config::inventory_path;
use crate::models::InventoryStore;
use crate::utils::read_json;
use anyhow::Result;

/// Loads `inventory.json` from the config directory; missing means empty.
pub(crate) fn load_inventory() -> Result<InventoryStore> {
    let path = inventory_path()?;
    if !path.exists() {
        return Ok(InventoryStore::default());
    }
    read_json(&path)
}
