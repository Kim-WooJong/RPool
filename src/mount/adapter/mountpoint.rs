//! Mountpoint validation and virtual-drive preflight checks.

use super::*;

/// Checks anchor, cache and target paths before mounting: absolute, non-overlapping, target
/// outside the workspace, empty and not already a mountpoint (Windows: unused drive letter).
/// Called by `MountProcess::start` before and after building the command.
pub(crate) fn validate_mountpoint(config: &MountConfig) -> Result<()> {
    if !config.anchor_dir.is_absolute() || !config.cache_dir.is_absolute() {
        bail!("mount source and cache must be absolute paths");
    }
    let source = config
        .anchor_dir
        .canonicalize()
        .context("mount source directory does not exist")?;
    if !source.is_dir() {
        bail!("mount source must be a directory");
    }
    let cache = normalized_existing_or_parent(&config.cache_dir)?;
    if source.starts_with(&cache) || cache.starts_with(&source) {
        bail!("mount source and VFS cache must not overlap");
    }
    #[cfg(windows)]
    {
        let value = config.target.to_string_lossy();
        let bytes = value.as_bytes();
        if bytes.len() != 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            bail!("Windows mount target must be an unused drive letter, such as R:");
        }
        // Unlike Path::exists, GetLogicalDrives also sees inaccessible/removable drives.
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetLogicalDrives() -> u32;
        }
        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            bail!("cannot determine occupied Windows drive letters");
        }
        let index = bytes[0].to_ascii_uppercase() - b'A';
        if mask & (1u32 << index) != 0 {
            bail!("mount drive letter is already in use");
        }
    }
    #[cfg(not(windows))]
    {
        if !config.target.is_absolute() {
            bail!("mount target must be an absolute directory");
        }
        let metadata =
            std::fs::symlink_metadata(&config.target).context("mount target must already exist")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("mount target must be a real directory, not a symlink");
        }
        let target = config.target.canonicalize()?;
        let workspace = source
            .parent()
            .context("mount source needs a workspace parent")?;
        if target.starts_with(workspace) || workspace.starts_with(&target) {
            bail!("mount target must be outside the entire workspace, including metadata");
        }
        if target != config.target {
            bail!("mount target must use its canonical path without symlink components");
        }
        if target.starts_with(&source)
            || source.starts_with(&target)
            || target.starts_with(&cache)
            || cache.starts_with(&target)
        {
            bail!("mount target must not overlap source or cache");
        }
        if std::fs::read_dir(&target)?.next().is_some() {
            bail!("mount target directory must be empty");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Some(parent) = target.parent() {
                if metadata.dev() != std::fs::metadata(parent)?.dev() {
                    bail!("mount target is already a filesystem mountpoint");
                }
            }
        }
    }
    Ok(())
}

/// Canonical path, or canonical parent plus file name when `path` does not exist yet.
pub(super) fn normalized_existing_or_parent(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path
        .parent()
        .context("cache needs a parent directory")?
        .canonicalize()?;
    let name = path.file_name().context("cache needs a directory name")?;
    Ok(parent.join(name))
}

/// Called before reconnecting an existing DAV endpoint to an orphaned rclone.
pub(crate) fn preflight_virtual(root: &Path) -> Result<()> {
    let path = root.join(".rpool/mount-process.json");
    reject_link(&path)?;
    if path.exists() {
        let lease: LeaseState = serde_json::from_slice(&std::fs::read(path)?)?;
        match lease {
            LeaseState::Launching => {
                bail!("uncertain previous mount launch; preserve cache and inspect process")
            }
            LeaseState::Running { pid } if process_alive(pid)? => {
                bail!("previous mount PID {pid} is still active")
            }
            _ => {}
        }
    }
    Ok(())
}
