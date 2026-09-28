use crate::models::Placement;
use crate::utils::{read_json, save_json_atomic};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GuiSettings {
    pub(crate) rclone: String,
    pub(crate) default_remote_path: String,
    pub(crate) remotes: Vec<String>,
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) placement: Placement,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            rclone: "rclone".to_string(),
            default_remote_path: "rpool".to_string(),
            remotes: Vec::new(),
            shard_mib: crate::config::constants::DEFAULT_SHARD_MIB,
            workers: crate::config::constants::DEFAULT_WORKERS,
            retries: crate::config::constants::DEFAULT_RETRIES,
            data_shards: crate::config::constants::DEFAULT_DATA_SHARDS,
            parity_shards: crate::config::constants::DEFAULT_PARITY_SHARDS,
            placement: Placement::RoundRobin,
        }
    }
}

pub(crate) fn load(startup_rclone: &str) -> GuiSettings {
    let mut settings = settings_path()
        .and_then(|path| read_json::<GuiSettings>(&path).ok())
        .unwrap_or_default();

    if startup_rclone != "rclone" {
        settings.rclone = startup_rclone.to_string();
    }
    settings
}

pub(crate) fn save(settings: &GuiSettings) -> Result<PathBuf, String> {
    let path =
        settings_path().ok_or_else(|| "cannot determine GUI settings directory".to_string())?;
    save_json_atomic(&path, settings)
        .map_err(|error| format!("failed to save GUI settings: {error:#}"))?;
    Ok(path)
}

pub(crate) fn settings_path() -> Option<PathBuf> {
    crate::config::gui_settings_path().ok()
}
