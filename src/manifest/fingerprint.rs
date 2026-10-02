//! Manifest fingerprints.

use crate::prelude::*;

/// BLAKE3 hex of the manifest's JSON serialization; used to compare replicas.
pub(crate) fn manifest_fingerprint(manifest: &Manifest) -> Result<String> {
    let bytes = serde_json::to_vec(manifest)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
