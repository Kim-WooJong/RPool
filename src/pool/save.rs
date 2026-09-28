use crate::config::pools_path;
use crate::models::PoolStore;
use crate::utils::save_json_atomic;
use anyhow::Result;
use std::path::PathBuf;

pub(crate) fn save_pool_store(store: &PoolStore) -> Result<PathBuf> {
    let path = pools_path()?;
    save_json_atomic(&path, store)?;
    Ok(path)
}
