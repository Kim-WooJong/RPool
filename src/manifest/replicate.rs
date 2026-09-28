use crate::manifest::{manifest_remotes, validate_manifest};
use crate::models::Manifest;
use crate::storage::writer::StorageWriter;
use crate::utils::remote_join;
use anyhow::Result;

pub(crate) fn replicate_manifest_with_storage(
    storage: &StorageWriter,
    manifest: &Manifest,
    requested_remotes: &[String],
    retries: u32,
) -> Result<Vec<String>> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    replicate_manifest_bytes_with_storage(storage, &bytes, requested_remotes, retries)
}

/// Pass-through replication validates but never reserializes source JSON.
pub(crate) fn replicate_manifest_bytes_with_storage(
    storage: &StorageWriter,
    bytes: &[u8],
    requested_remotes: &[String],
    retries: u32,
) -> Result<Vec<String>> {
    let manifest: Manifest = serde_json::from_slice(bytes)?;
    validate_manifest(&manifest)?;
    let remotes = if requested_remotes.is_empty() {
        manifest_remotes(&manifest)
    } else {
        requested_remotes.to_vec()
    };
    for remote in &remotes {
        storage.ensure_destination(remote)?;
    }
    let mut written = Vec::with_capacity(remotes.len());
    for remote in remotes {
        let target = remote_join(&remote, &format!("{}/manifest.json", manifest.archive_id));
        storage.write_bytes(&target, bytes, retries)?;
        written.push(target);
    }
    Ok(written)
}
