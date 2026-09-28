use crate::models::Placement;
use crate::utils::{read_json, save_json_atomic};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GuiSettings {
    pub(crate) encryption: crate::config_sync::provision::EncryptionDefaults,
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
            encryption: Default::default(),
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

    if settings.encryption.validate().is_err() {
        settings.encryption = Default::default();
    }
    if startup_rclone != "rclone" {
        settings.rclone = startup_rclone.to_string();
    }
    settings
}

pub(crate) fn save(settings: &GuiSettings) -> Result<PathBuf, String> {
    settings.encryption.validate().map_err(|e| e.to_string())?;
    let path =
        settings_path().ok_or_else(|| "cannot determine GUI settings directory".to_string())?;
    save_json_atomic(&path, settings)
        .map_err(|error| format!("failed to save GUI settings: {error:#}"))?;
    Ok(path)
}

pub(crate) fn settings_path() -> Option<PathBuf> {
    crate::config::gui_settings_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_gain_encryption_defaults() {
        let settings: GuiSettings =
            serde_json::from_str(r#"{"rclone":"custom-rclone","workers":7}"#).unwrap();
        assert_eq!(settings.rclone, "custom-rclone");
        assert_eq!(settings.workers, 7);
        assert_eq!(settings.encryption, Default::default());
        assert_eq!(settings.encryption.entropy_bits, 1024);
    }
    #[test]
    fn encryption_settings_round_trip_and_partial_migration() {
        let mut settings = GuiSettings::default();
        settings.encryption.entropy_bits = 512;
        settings.encryption.filename_encryption = "off".into();
        settings.encryption.directory_encryption = false;
        settings.encryption.root = "nested/crypt".into();
        let decoded: GuiSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(decoded.encryption, settings.encryption);
        let partial: GuiSettings =
            serde_json::from_str(r#"{"encryption":{"root":"custom"}}"#).unwrap();
        assert_eq!(partial.encryption.root, "custom");
        assert_eq!(partial.encryption.entropy_bits, 1024);
    }
    #[test]
    fn invalid_encryption_settings_are_rejected_before_saving() {
        let mut settings = GuiSettings::default();
        settings.encryption.root = "../escape".into();
        assert!(save(&settings)
            .unwrap_err()
            .contains("relative provider folder"));
    }
}
