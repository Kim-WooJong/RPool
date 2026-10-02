//! Set/remove operations on configured remote default paths.
use crate::remote_root::{load_remote_root_store, remote_name, save_remote_root_store};
use anyhow::{bail, Result};
use std::path::PathBuf;

/// Stores `path` (trimmed, trailing `/` removed except for `/`) as the default
/// path of `remote`'s name and saves the store; returns the file path.
/// Used by `remote-root set` and GUI Settings.
pub(crate) fn set_remote_root(remote: &str, path: &str) -> Result<PathBuf> {
    let name = remote_name(remote)?.to_string();
    let path = normalize_root(path)?;
    let mut store = load_remote_root_store()?;
    store.roots.insert(name, path);
    save_remote_root_store(&store)
}

/// Removes the default path of `remote`'s name and saves the store; errors if
/// none was configured. Used by `remote-root remove` and GUI Settings.
pub(crate) fn remove_remote_root(remote: &str) -> Result<PathBuf> {
    let name = remote_name(remote)?.to_string();
    let mut store = load_remote_root_store()?;
    if store.roots.remove(&name).is_none() {
        bail!("no configured default path for remote: {name}:");
    }
    save_remote_root_store(&store)
}

/// Trims a root path and strips trailing slashes (keeping `/` as is); an empty
/// path is rejected so users remove the override instead.
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
