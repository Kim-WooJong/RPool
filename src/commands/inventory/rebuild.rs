//! `rpool inventory rebuild`: rebuilds the inventory from local manifests.
use crate::config::inventory_path;
use crate::inventory::rebuild_from_directory;
use anyhow::Result;
use std::path::Path;

/// Replaces the inventory with the manifests found recursively under
/// `directory` and prints indexed/skipped counts and the inventory path.
pub(crate) fn run(directory: &Path) -> Result<()> {
    let (store, skipped) = rebuild_from_directory(directory)?;
    println!("indexed={}", store.entries.len());
    println!("skipped={skipped}");
    println!("inventory={}", inventory_path()?.display());
    Ok(())
}
