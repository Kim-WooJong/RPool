use crate::manifest::validate_manifest;
use crate::models::Manifest;
use crate::storage::reader::StorageReader;
use crate::utils::remote_join;
use anyhow::{bail, Result};
use std::io::Write;
use std::path::Path;

pub(crate) fn recover_manifest_with_storage(
    reader: &StorageReader,
    archive_id: &str,
    remotes: &[String],
    output: &Path,
) -> Result<String> {
    if remotes.is_empty() {
        bail!("manifest recovery requires at least one --remote or --pool");
    }

    let mut errors = Vec::new();
    for remote in remotes {
        let object = remote_join(remote, &format!("{archive_id}/manifest.json"));
        let result = (|| -> Result<Vec<u8>> {
            let bytes = reader.read_metadata(&object)?;
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            validate_manifest(&manifest)?;
            if manifest.archive_id != archive_id {
                bail!(
                    "archive id mismatch: expected {}, found {}",
                    archive_id,
                    manifest.archive_id
                );
            }
            Ok(bytes)
        })();

        match result {
            Ok(bytes) => {
                let parent = output
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                std::fs::create_dir_all(parent)?;
                let mut staged = tempfile::NamedTempFile::new_in(parent)?;
                staged.write_all(&bytes)?;
                staged.flush()?;
                staged.persist(output)?;
                return Ok(object);
            }
            Err(error) => errors.push(format!("{object}: {error:#}")),
        }
    }

    bail!("no valid manifest replica found:\n{}", errors.join("\n"))
}
