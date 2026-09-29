use anyhow::{anyhow, bail, Context, Result};
use std::env;
use std::path::PathBuf;

pub(crate) fn app_config_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("RPOOL_CONFIG_DIR") {
        return checked_override_dir(PathBuf::from(path));
    }
    let path = if cfg!(target_os = "windows") {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("rpool"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME").map(PathBuf::from).map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("rpool")
        })
    } else if let Some(base) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(base.join("rpool"))
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".config").join("rpool"))
    };

    path.ok_or_else(|| anyhow!("cannot determine rpool configuration directory"))
}

fn checked_override_dir(path: PathBuf) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("RPOOL_CONFIG_DIR must be an absolute directory");
    }
    let metadata = std::fs::symlink_metadata(&path).context("RPOOL_CONFIG_DIR must exist")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("RPOOL_CONFIG_DIR must be a real directory");
    }
    Ok(path.canonicalize()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_requires_existing_absolute_real_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            checked_override_dir(root.path().into()).unwrap(),
            root.path().canonicalize().unwrap()
        );
        assert!(checked_override_dir(PathBuf::from("relative")).is_err());
        assert!(checked_override_dir(root.path().join("missing")).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(root.path(), &link).unwrap();
            assert!(checked_override_dir(link).is_err());
        }
    }
}

pub(crate) fn pools_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("pools.json"))
}

pub(crate) fn inventory_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("inventory.json"))
}

pub(crate) fn history_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("history.jsonl"))
}

pub(crate) fn gui_settings_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("gui.json"))
}

pub(crate) fn remote_roots_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("remote_roots.json"))
}

pub(crate) fn integrity_snapshot_path() -> Result<PathBuf> {
    Ok(app_config_dir()?.join("integrity.json"))
}
