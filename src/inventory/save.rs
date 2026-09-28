use crate::config::inventory_path;
use crate::models::InventoryStore;
use anyhow::Result;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) fn save_inventory(store: &InventoryStore) -> Result<PathBuf> {
    let path = inventory_path()?;
    save_inventory_at(&path, store)?;
    Ok(path)
}

fn save_inventory_at(path: &Path, store: &InventoryStore) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(store)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    // Never remove the old index first: a killed process leaves old or new,
    // not a missing/truncated inventory. Tempfile handles replacement on Windows.
    staged.persist(path)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_replace_is_complete_and_failed_publish_keeps_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory.json");
        let store = InventoryStore::default();
        save_inventory_at(&path, &store).unwrap();
        save_inventory_at(&path, &store).unwrap();
        let parsed: InventoryStore =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed.entries.len(), store.entries.len());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let occupied = dir.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("keep"), "original").unwrap();
        assert!(save_inventory_at(&occupied, &store).is_err());
        assert_eq!(
            std::fs::read_to_string(occupied.join("keep")).unwrap(),
            "original"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
