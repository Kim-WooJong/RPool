//! Adding/replacing and removing saved pools.

use crate::models::PoolDefinition;
use crate::pool::{load_pool_store, save_pool_store, validate_pool, validate_pool_name};
use crate::remote_root::apply_remote_roots;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use anyhow::{bail, Result};
use std::path::PathBuf;

/// [`upsert_pool_with_admin`] with the real rclone admin. Called by
/// `rpool pool set` and the GUI pool editor.
pub(crate) fn upsert_pool(rclone: &str, name: &str, pool: PoolDefinition) -> Result<PathBuf> {
    upsert_pool_with_admin(&RcloneAdmin::inherited(rclone), name, pool)
}
/// Validates `name` and `pool`, requires every (root-resolved) remote to be
/// an encrypted remote, then saves the pool; returns the store path.
pub(crate) fn upsert_pool_with_admin(
    admin: &dyn BackendAdmin,
    name: &str,
    pool: PoolDefinition,
) -> Result<PathBuf> {
    validate_pool_name(name)?;
    validate_pool(&pool)?;
    let resolved_remotes = apply_remote_roots(pool.remotes.clone())?;
    for remote in &resolved_remotes {
        admin.ensure_encrypted(remote)?;
    }
    let mut store = load_pool_store()?;
    store.pools.insert(name.to_string(), pool);
    save_pool_store(&store)
}

/// Removes pool `name` with its retention and drive-cleanup settings;
/// errors if it does not exist. Called by `rpool pool remove` and the GUI.
pub(crate) fn remove_pool(name: &str) -> Result<PathBuf> {
    validate_pool_name(name)?;
    let mut store = load_pool_store()?;
    if store.pools.remove(name).is_none() {
        bail!("pool not found: {name}");
    }
    store.retention.remove(name);
    store.drive_cleanup.remove(name);
    save_pool_store(&store)
}
