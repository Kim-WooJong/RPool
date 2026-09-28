use crate::manifest::{load_manifest_bytes_with_storage, replicate_manifest_bytes_with_storage};
use crate::pool::resolve_target_remotes;
use crate::storage::{reader::StorageReader, writer::StorageWriter};
use anyhow::Result;

pub(crate) fn run(
    rclone: &str,
    manifest_source: &str,
    pool: Option<&str>,
    remotes: Vec<String>,
    retries: u32,
) -> Result<()> {
    let bytes = load_manifest_bytes_with_storage(&StorageReader::rclone(rclone), manifest_source)?;
    let requested = resolve_target_remotes(pool, remotes)?;
    let written = replicate_manifest_bytes_with_storage(
        &StorageWriter::rclone(rclone),
        &bytes,
        &requested,
        retries,
    )?;
    for object in written {
        println!("replica={object}");
    }
    Ok(())
}
