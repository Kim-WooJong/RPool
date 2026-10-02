//! Loads the saved pool store (`pools.json`).

use crate::config::pools_path;
use crate::models::PoolStore;
use crate::utils::read_json;
use anyhow::Result;

/// Reads `pools.json`; a missing file is an empty store. Used wherever a
/// saved pool is looked up (mount, CLI, GUI).
pub(crate) fn load_pool_store() -> Result<PoolStore> {
    let path = pools_path()?;
    if !path.exists() {
        return Ok(PoolStore::default());
    }
    read_json(&path)
}
