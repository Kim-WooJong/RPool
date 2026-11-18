//! Per-PC shared operation lease / exclusive individual-location edit fence.
//! Kernel owned, never removes the lock inode or terminates another process.
use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};

pub(crate) fn acquire(edit: bool) -> Result<File> {
    let dir = crate::config::app_config_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("location-use.lock");
    if fs::symlink_metadata(&path).is_ok() {
        let meta = fs::symlink_metadata(&path)?;
        anyhow::ensure!(meta.is_file() && !meta.file_type().is_symlink(), "location-use lock must be a regular file");
    }
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    if edit { file.try_lock().context("Another RPool operation is active on this PC; stop mounts/uploads/jobs before editing locations")?; }
    else { file.try_lock_shared().context("A storage location is being edited on this PC; wait until it finishes")?; }
    Ok(file)
}
