use crate::prelude::*;
use crate::storage::reader::StorageReader;

pub(crate) fn load_manifest(rclone: &str, source: &str) -> Result<Manifest> {
    load_manifest_with_storage(&StorageReader::rclone(rclone), source)
}
pub(crate) fn load_manifest_with_storage(reader: &StorageReader, source: &str) -> Result<Manifest> {
    let bytes = load_manifest_bytes_with_storage(reader, source)?;
    serde_json::from_slice(&bytes).context("invalid rpool manifest")
}

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
