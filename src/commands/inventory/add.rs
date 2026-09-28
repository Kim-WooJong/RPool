use crate::config::inventory_path;
use crate::inventory::add_manifest;
use anyhow::Result;

pub(crate) fn run(rclone: &str, manifest: &str) -> Result<()> {
    add_manifest(rclone, manifest)?;
    println!("indexed={manifest}");
    println!("inventory={}", inventory_path()?.display());
    Ok(())
}
