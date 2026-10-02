//! Expansion of rclone remote strings with their configured default path.
use crate::models::RemoteRootStore;
use crate::remote_root::load_remote_root_store;
use anyhow::{anyhow, Result};

/// Returns the remote name before the first `:` (or the whole trimmed input
/// without a colon); errors when the name is empty.
pub(crate) fn remote_name(remote: &str) -> Result<&str> {
    let value = remote.trim();
    let name = value.split_once(':').map(|(name, _)| name).unwrap_or(value);
    if name.trim().is_empty() {
        return Err(anyhow!("rclone remote name cannot be empty: {remote}"));
    }
    Ok(name)
}

/// Expands a bare `name:` remote with its configured root (loaded from disk);
/// a remote that already has a path is returned trimmed and unchanged.
/// Errors when there is no `:` or the name is empty.
pub(crate) fn apply_remote_root(remote: &str) -> Result<String> {
    let (name, path) = remote
        .split_once(':')
        .ok_or_else(|| anyhow!("expected an rclone remote path, got: {remote}"))?;
    if name.trim().is_empty() {
        return Err(anyhow!("rclone remote name cannot be empty: {remote}"));
    }
    if !path.trim().is_empty() {
        return Ok(remote.trim().to_string());
    }

    apply_remote_root_with_store(remote, &load_remote_root_store()?)
}

/// Resolve using an operation-scoped snapshot; explicit paths remain unchanged.
pub(crate) fn apply_remote_root_with_store(
    remote: &str,
    store: &RemoteRootStore,
) -> Result<String> {
    let (name, path) = remote
        .split_once(':')
        .ok_or_else(|| anyhow!("expected an rclone remote path, got: {remote}"))?;
    if name.trim().is_empty() {
        return Err(anyhow!("rclone remote name cannot be empty: {remote}"));
    }
    if !path.trim().is_empty() {
        return Ok(remote.trim().to_string());
    }
    let Some(root) = store.roots.get(name) else {
        return Ok(format!("{name}:"));
    };
    Ok(join_remote_root(name, root))
}

/// Applies [`apply_remote_root`] to every remote; fails on the first error.
/// Used when pools, policies or migration targets resolve their remotes.
pub(crate) fn apply_remote_roots(remotes: Vec<String>) -> Result<Vec<String>> {
    remotes
        .into_iter()
        .map(|remote| apply_remote_root(&remote))
        .collect()
}

/// Formats the expanded remote `name:root`.
fn join_remote_root(name: &str, root: &str) -> String {
    format!("{name}:{root}")
}
