use crate::config::remote_roots_path;
use crate::models::RemoteRootStore;
use crate::utils::read_json;
use anyhow::{bail, Result};

pub(crate) fn load_remote_root_store() -> Result<RemoteRootStore> {
    let path = remote_roots_path()?;
    if !path.exists() {
        return Ok(RemoteRootStore::default());
    }
    let store: RemoteRootStore = read_json(&path)?;
    if store.version != 1 {
        bail!("unsupported remote root config version: {}", store.version);
    }
    Ok(store)
}
