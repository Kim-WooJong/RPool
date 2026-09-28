use crate::remote_root::{load_remote_root_store, remote_name, save_remote_root_store};
use anyhow::{bail, Result};
use std::path::PathBuf;

pub(crate) fn set_remote_root(remote: &str, path: &str) -> Result<PathBuf> {
    let name = remote_name(remote)?.to_string();
    let path = normalize_root(path)?;
    let mut store = load_remote_root_store()?;
    store.roots.insert(name, path);
    save_remote_root_store(&store)
}

pub(crate) fn remove_remote_root(remote: &str) -> Result<PathBuf> {
    let name = remote_name(remote)?.to_string();
    let mut store = load_remote_root_store()?;
    if store.roots.remove(&name).is_none() {
        bail!("no configured default path for remote: {name}:");
    }
    save_remote_root_store(&store)
}

fn normalize_root(path: &str) -> Result<String> {
    let value = path.trim();
    if value.is_empty() {
        bail!("remote default path cannot be empty; remove the override instead");
    }
    if value == "/" {
        return Ok(value.to_string());
    }
    Ok(value.trim_end_matches('/').to_string())
}
