use crate::config::remote_roots_path;
use crate::models::RemoteRootStore;
use crate::utils::save_json_atomic;
use anyhow::Result;
use std::path::PathBuf;

pub(crate) fn save_remote_root_store(store: &RemoteRootStore) -> Result<PathBuf> {
    let path = remote_roots_path()?;
    save_json_atomic(&path, store)?;
    Ok(path)
}
