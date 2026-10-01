//! Offline export of retained spool writes (`--recover-spool`).

use super::*;

pub(super) fn checked_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("managed directory must not be a symlink");
    }
    Ok(())
}

pub(crate) fn recover_spool(root: &Path) -> Result<Vec<PathBuf>> {
    checked_directory(root)?;
    crate::mount::adapter::preflight_virtual(root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("virtual.lock"))?;
    lock.try_lock()
        .context("stop the virtual drive before recovery")?;
    checked_directory(&root.join("spool"))?;
    let output = root.join("recovered-writes");
    fs::create_dir_all(&output)?;
    checked_directory(&output)?;
    let mut paths = vec![];
    for entry in fs::read_dir(root.join("spool"))? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid spool identity");
        }
        checked_directory(&entry.path())?;
        let source = entry.path().join("content");
        if !source.exists() {
            continue;
        }
        let meta = fs::symlink_metadata(&source)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("invalid spool file");
        }
        let receipt: Option<Intent> =
            crate::utils::read_json(&entry.path().join("intent.json")).ok();
        let verified = receipt.as_ref().is_some_and(|i| {
            i.id == id
                && i.spool.as_deref() == Some(&id)
                && i.size == meta.len()
                && crate::utils::hash_file_range(&source, 0, meta.len())
                    .is_ok_and(|hash| hash == i.hash)
        });
        let target = output.join(format!(
            "{id}.{}.bin",
            if verified { "sealed" } else { "partial" }
        ));
        let mut temp = tempfile::NamedTempFile::new_in(&output)?;
        std::io::copy(&mut File::open(&source)?, &mut temp)?;
        temp.as_file().sync_all()?;
        if target.exists() {
            if crate::utils::hash_file_range(&target, 0, fs::metadata(&target)?.len())?
                != crate::utils::hash_file_range(temp.path(), 0, temp.as_file().metadata()?.len())?
            {
                bail!("recovery output already exists with different bytes");
            }
        } else {
            temp.persist_noclobber(&target).map_err(|e| e.error)?;
        }
        if let Some(receipt) = receipt {
            durable_json(&output.join(format!("{id}.json")), &receipt)?;
        }
        paths.push(target);
    }
    #[cfg(unix)]
    File::open(output)?.sync_all()?;
    Ok(paths)
}
