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
    let targets: Vec<String> = remotes
        .iter()
        .map(|remote| remote_join(remote, &format!("{}/manifest.json", manifest.archive_id)))
        .collect();
    // Replicas are independent verified writes, so one slow provider no longer
    // delays the others: every replica is written on its own thread. All
    // in-flight writes are allowed to finish; afterwards the error of the first
    // failing remote in list order is returned unchanged (the sequential loop
    // returned the same error, but skipped the later replicas). The returned
    // targets keep the remote-list order; callers treat element 0 as primary.
    let results: Vec<Result<()>> = std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|target| scope.spawn(move || storage.write_bytes(target, bytes, retries)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    for result in results {
        result?;
    }
    Ok(targets)
}

#[cfg(test)]
#[path = "replicate_tests.rs"]
mod tests;
