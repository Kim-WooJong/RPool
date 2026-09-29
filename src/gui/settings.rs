use crate::models::Placement;
use crate::utils::{read_json, save_json_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GuiSettings {
    pub(crate) mount_cache: MountCacheSettings,
    pub(crate) mount_profiles: BTreeMap<String, MountProfile>,
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
            mount_cache: MountCacheSettings::default(),
            mount_profiles: BTreeMap::new(),
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

/// Machine-local mount inputs, isolated by upload pool. Runtime/recovery state is never saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MountProfile {
    pub(crate) workspace: String,
    pub(crate) mountpoint: String,
    pub(crate) shared_root: String,
    pub(crate) worker_name: String,
    pub(crate) manifests: Vec<String>,
    pub(crate) interval_seconds: u64,
    pub(crate) bounded_shared: bool,
    pub(crate) pool_sync: bool,
    pub(crate) pool_retention: bool,
    pub(crate) pool_history_limit: u32,
    pub(crate) pool_history_override: bool,
    pub(crate) shared_coordinator: bool,
    pub(crate) shared_keep_previous: usize,
    pub(crate) cache: MountCacheSettings,
}

impl Default for MountProfile {
    fn default() -> Self {
        Self {
            workspace: Default::default(),
            mountpoint: if cfg!(windows) {
                "R:".into()
            } else {
                String::new()
            },
            shared_root: Default::default(),
            worker_name: Default::default(),
            manifests: Default::default(),
            interval_seconds: 30,
            bounded_shared: Default::default(),
            pool_sync: true,
            pool_retention: Default::default(),
            pool_history_limit: Default::default(),
            pool_history_override: Default::default(),
            shared_coordinator: Default::default(),
            shared_keep_previous: Default::default(),
            cache: Default::default(),
        }
    }
}

/// Machine-local cache preferences, never a cloud retention/deletion policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MountCacheSettings {
    pub(crate) online_drive: bool,
    pub(crate) shard_gib: u64,
    pub(crate) native_gib: u64,
    pub(crate) min_free_gib: u64,
    pub(crate) spool_gib: u64,
}

impl Default for MountCacheSettings {
    fn default() -> Self {
        Self {
            online_drive: true,
            shard_gib: 10,
            native_gib: 10,
            min_free_gib: 2,
            spool_gib: 64,
        }
    }
}

impl MountCacheSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if [self.shard_gib, self.native_gib, self.spool_gib]
            .iter()
            .any(|value| !(1..=1048576).contains(value))
            || self.min_free_gib > 1048576
        {
            return Err(
                "Cache limits must be 1–1048576 GiB (free-space target may be zero).".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn load(startup_rclone: &str) -> GuiSettings {
    let mut settings = settings_path()
        .and_then(|path| read_json::<GuiSettings>(&path).ok())
        .unwrap_or_default();

    if settings.encryption.validate().is_err() {
        settings.encryption = Default::default();
    }
    if settings.mount_cache.validate().is_err() {
        let online_drive = settings.mount_cache.online_drive;
        settings.mount_cache = MountCacheSettings::default();
        settings.mount_cache.online_drive = online_drive;
    }
    for profile in settings.mount_profiles.values_mut() {
        if profile.cache.validate().is_err() {
            let online_drive = profile.cache.online_drive;
            profile.cache = MountCacheSettings::default();
            profile.cache.online_drive = online_drive;
        }
        profile.interval_seconds = profile.interval_seconds.clamp(2, 86400);
        profile.pool_history_limit = profile.pool_history_limit.min(10000);
        profile.shared_keep_previous = profile.shared_keep_previous.min(100);
    }
    if startup_rclone != "rclone" {
        settings.rclone = startup_rclone.to_string();
    }
    settings
}

pub(crate) fn save(settings: &GuiSettings) -> Result<PathBuf, String> {
    settings.mount_cache.validate()?;
    for profile in settings.mount_profiles.values() {
        profile.cache.validate()?;
    }
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
    fn invalid_pool_profile_cache_is_rejected_before_saving() {
        let mut settings = GuiSettings::default();
        let mut profile = MountProfile::default();
        profile.cache.spool_gib = 0;
        settings.mount_profiles.insert("pool".into(), profile);
        assert!(save(&settings).unwrap_err().contains("Cache limits"));
    }

    #[test]
    fn cache_preferences_persist_and_old_settings_default_online() {
        let old: GuiSettings = serde_json::from_str(r#"{"workers":7}"#).unwrap();
        assert!(old.mount_cache.online_drive);
        assert_eq!(old.mount_cache, MountCacheSettings::default());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gui-settings.json");
        let mut settings = old;
        settings.mount_cache = MountCacheSettings {
            online_drive: false,
            shard_gib: 4,
            native_gib: 5,
            min_free_gib: 0,
            spool_gib: 8,
        };
        save_json_atomic(&path, &settings).unwrap();
        let loaded: GuiSettings = read_json(&path).unwrap();
        assert_eq!(loaded.mount_cache, settings.mount_cache);
        assert_eq!(loaded.workers, 7);
        assert!(loaded.mount_cache.validate().is_ok());
    }

    #[test]
    fn invalid_cache_limits_are_rejected_before_save() {
        for (shard, native, free, spool) in
            [(0, 1, 0, 1), (1, 0, 0, 1), (1, 1, 1048577, 1), (1, 1, 0, 0)]
        {
            let mut settings = GuiSettings::default();
            settings.mount_cache = MountCacheSettings {
                online_drive: true,
                shard_gib: shard,
                native_gib: native,
                min_free_gib: free,
                spool_gib: spool,
            };
            assert!(save(&settings).unwrap_err().contains("Cache limits"));
        }
    }
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
        settings.encryption.entropy_bits = 12;
        assert!(save(&settings).unwrap_err().contains("entropy"));
    }
}
