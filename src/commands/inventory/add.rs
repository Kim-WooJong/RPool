//! `rpool inventory add`: indexes one manifest in the local inventory.
use crate::config::inventory_path;
use crate::inventory::add_manifest;
use anyhow::Result;

/// Adds or refreshes `manifest` (local or rclone path) in the inventory and
/// prints the inventory file path. Called via `commands::inventory::add`.
pub(crate) fn run(rclone: &str, manifest: &str) -> Result<()> {
    add_manifest(rclone, manifest)?;
    println!("indexed={manifest}");
    println!("inventory={}", inventory_path()?.display());
    Ok(())
}
