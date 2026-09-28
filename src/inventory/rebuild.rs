use crate::inventory::{entry_from_manifest, save_inventory};
use crate::manifest::validate_manifest;
use crate::models::{InventoryStore, Manifest};
use anyhow::{bail, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn rebuild_from_directory(root: &Path) -> Result<(InventoryStore, usize)> {
    if !root.is_dir() {
        bail!("inventory rebuild root is not a directory: {}", root.display());
    }

    let mut candidates = Vec::new();
    collect_candidates(root, &mut candidates)?;
    let mut store = InventoryStore::default();
    let mut skipped = 0usize;

    for path in candidates {
        let result = (|| -> Result<Manifest> {
            let bytes = fs::read(&path)?;
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            validate_manifest(&manifest)?;
            Ok(manifest)
        })();
        match result {
            Ok(manifest) => {
                let entry = entry_from_manifest(&manifest, path.to_string_lossy().into_owned());
                store.entries.insert(entry.archive_id.clone(), entry);
            }
            Err(_) => skipped += 1,
        }
    }

    save_inventory(&store)?;
    Ok((store, skipped))
}

fn collect_candidates(root: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_candidates(&path, output)?;
        } else if file_type.is_file() && is_manifest_candidate(&path) {
            output.push(path);
        }
    }
    Ok(())
}

fn is_manifest_candidate(path: &Path) -> bool {
    let name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default();
    name.ends_with(".rpool.json") || name == "manifest.json"
}
