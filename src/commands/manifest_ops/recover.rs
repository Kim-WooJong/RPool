//! `rpool manifest recover`: fetches a manifest replica back from the providers.
use crate::manifest::recover_manifest_with_storage;
use crate::pool::resolve_target_remotes;
use crate::storage::reader::StorageReader;
use anyhow::Result;
use std::path::PathBuf;

/// Downloads the first usable manifest replica of `archive_id` from the pool's
/// or the given remotes to `output` (default `<archive_id>.rpool.json`), indexes
/// it in the inventory (best effort) and prints where it came from.
pub(crate) fn run(
    rclone: &str,
    archive_id: &str,
    pool: Option<&str>,
    remotes: Vec<String>,
    output: Option<PathBuf>,
) -> Result<()> {
    let remotes = resolve_target_remotes(pool, remotes)?;
    let output = output.unwrap_or_else(|| PathBuf::from(format!("{archive_id}.rpool.json")));
    let reader = StorageReader::rclone(rclone);
    let source = recover_manifest_with_storage(&reader, archive_id, &remotes, &output)?;
    let output_source = output.to_string_lossy().into_owned();
    if let Err(error) = crate::inventory::add_manifest(rclone, &output_source) {
        eprintln!("[inventory] recovered manifest saved but index update failed: {error:#}");
    }
    println!("manifest={}", output.display());
    println!("recovered_from={source}");
    Ok(())
}
