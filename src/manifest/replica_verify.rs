use crate::manifest::{manifest_fingerprint, manifest_remotes, validate_manifest};
use crate::models::Manifest;
use crate::storage::reader::StorageReader;
use crate::utils::remote_join;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ManifestReplicaReport {
    pub(crate) remote: String,
    pub(crate) object: String,
    pub(crate) healthy: bool,
    pub(crate) message: String,
}

pub(crate) fn verify_manifest_replicas_with_storage(
    reader: &StorageReader,
    reference: &Manifest,
    requested_remotes: &[String],
) -> Result<Vec<ManifestReplicaReport>> {
    validate_manifest(reference)?;
    let expected = manifest_fingerprint(reference)?;
    let remotes = if requested_remotes.is_empty() {
        manifest_remotes(reference)
    } else {
        requested_remotes.to_vec()
    };

    let mut reports = Vec::with_capacity(remotes.len());
    for remote in remotes {
        let object = remote_join(&remote, &format!("{}/manifest.json", reference.archive_id));
        let result = (|| -> Result<()> {
            let bytes = reader.read_metadata(&object)?;
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            validate_manifest(&manifest)?;
            let found = manifest_fingerprint(&manifest)?;
            if found != expected {
                anyhow::bail!("manifest fingerprint mismatch");
            }
            Ok(())
        })();

        match result {
            Ok(()) => reports.push(ManifestReplicaReport {
                remote,
                object,
                healthy: true,
                message: "ok".to_string(),
            }),
            Err(error) => reports.push(ManifestReplicaReport {
                remote,
                object,
                healthy: false,
                message: format!("{error:#}"),
            }),
        }
    }
    Ok(reports)
}
