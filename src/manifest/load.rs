//! Loading manifests from a local file or a remote object.

use crate::prelude::*;
use crate::storage::reader::StorageReader;

/// Loads a manifest via rclone; see [`load_manifest_with_storage`].
pub(crate) fn load_manifest(rclone: &str, source: &str) -> Result<Manifest> {
    load_manifest_with_storage(&StorageReader::rclone(rclone), source)
}
/// Reads and parses a manifest (not validated) from `source`.
pub(crate) fn load_manifest_with_storage(reader: &StorageReader, source: &str) -> Result<Manifest> {
    let bytes = load_manifest_bytes_with_storage(reader, source)?;
    serde_json::from_slice(&bytes).context("invalid rpool manifest")
}

/// Raw manifest bytes: from the local file when `source` exists on disk, else
/// read from storage as a remote object.
pub(crate) fn load_manifest_bytes_with_storage(
    reader: &StorageReader,
    source: &str,
) -> Result<Vec<u8>> {
    let local = Path::new(source);
    let bytes = if local.exists() {
        fs::read(local).with_context(|| format!("cannot read manifest: {}", local.display()))?
    } else {
        reader.read_metadata(source)?
    };
    Ok(bytes)
}
