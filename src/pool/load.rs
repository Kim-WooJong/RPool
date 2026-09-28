use crate::config::pools_path;
use crate::models::PoolStore;
use crate::utils::read_json;
use anyhow::Result;

pub(crate) fn load_pool_store() -> Result<PoolStore> {
    let path = pools_path()?;
    if !path.exists() {
        return Ok(PoolStore::default());
    }
    read_json(&path)
}
