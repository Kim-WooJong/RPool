use crate::inventory::rebuild_from_directory;
use crate::config::inventory_path;
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(directory: &Path) -> Result<()> {
    let (store, skipped) = rebuild_from_directory(directory)?;
    println!("indexed={}", store.entries.len());
    println!("skipped={skipped}");
    println!("inventory={}", inventory_path()?.display());
    Ok(())
}
