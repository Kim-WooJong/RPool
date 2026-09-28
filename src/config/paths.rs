use anyhow::{anyhow, Result};
use std::env;
use std::path::PathBuf;

pub(crate) fn app_config_dir() -> Result<PathBuf> {
    let path = if cfg!(target_os = "windows") {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("rpool"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library").join("Application Support").join("rpool"))
    } else if let Some(base) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(base.join("rpool"))
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".config").join("rpool"))
    };

    path.ok_or_else(|| anyhow!("cannot determine rpool configuration directory"))
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
