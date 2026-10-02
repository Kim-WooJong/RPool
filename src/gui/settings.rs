//! GUI settings of this PC (`gui_settings_path()`): language, rclone path,
//! new-pool defaults, encryption defaults and per-pool mount profiles.
//! Loaded once at startup (`state::persistence`), saved by the Settings,
//! Network and Drive screens; invalid parts fall back to defaults on load.
use crate::models::Placement;
use crate::utils::{read_json, save_json_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Everything the GUI remembers between runs, held in `GuiState::settings`.
/// Missing fields take their defaults (`#[serde(default)]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct GuiSettings {
    /// GUI language (English by default).
    pub(crate) language: crate::gui::i18n::Language,
    /// Cache budgets for a mount when no pool profile exists yet.
    pub(crate) mount_cache: MountCacheSettings,
    /// Mount inputs saved per pool name.
    pub(crate) mount_profiles: BTreeMap<String, MountProfile>,
    /// Options for crypt remotes RPool creates (Settings › Encryption).
    pub(crate) encryption: crate::config_sync::provision::EncryptionDefaults,
    /// rclone executable name or path; a non-default startup value overrides it.
    pub(crate) rclone: String,
    /// Global crypt folder fallback when a remote has no per-remote default path.
    pub(crate) default_remote_path: String,
    /// Saved upload destinations for manual uploads; also prefill new pools.
    pub(crate) remotes: Vec<String>,
    /// Shard size of new pools, MiB.
    pub(crate) shard_mib: u64,
    /// Shard transfers of this PC (Settings › Network); also new pools' `workers`.
    pub(crate) workers: usize,
    /// Retry count for rclone transfers.
    pub(crate) retries: u32,
    /// Data shards (K) of new pools.
    pub(crate) data_shards: usize,
    /// Parity shards (M) of new pools.
    pub(crate) parity_shards: usize,
    /// Placement of new pools.
    pub(crate) placement: Placement,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            language: Default::default(),
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
    /// Persistent local workspace folder.
    pub(crate) workspace: String,
    /// Drive letter or mount folder.
    pub(crate) mountpoint: String,
    /// Display name in pool-sync conflicts (`--pool-worker`). Older settings
    /// stored it as `worker_name`.
    #[serde(alias = "worker_name")]
    pub(crate) pc_name: String,
    /// Archive manifests applied at the next start.
    pub(crate) manifests: Vec<String>,
    /// Background sync interval, seconds (clamped to 2..=86400 on load).
    pub(crate) interval_seconds: u64,
    /// Cache budgets of this pool's mount.
    pub(crate) cache: MountCacheSettings,
    /// Filesystem frontend.
    pub(crate) frontend: crate::cli::Frontend,
    /// Mount read-only with a native frontend.
    pub(crate) native_read_only: bool,
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
            pc_name: Default::default(),
            manifests: Default::default(),
            interval_seconds: 30,
            cache: Default::default(),
            frontend: Default::default(),
            native_read_only: Default::default(),
        }
    }
}

/// Machine-local cache preferences, never a cloud retention/deletion policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MountCacheSettings {
    /// Clean shard cache budget, GiB.
    pub(crate) shard_gib: u64,
    /// Native / OS mount cache target, GiB.
    pub(crate) native_gib: u64,
    /// Disk space to leave free, GiB (may be 0).
    pub(crate) min_free_gib: u64,
    /// Pending-write (spool) limit, GiB.
    pub(crate) spool_gib: u64,
}

impl Default for MountCacheSettings {
    fn default() -> Self {
        Self {
            shard_gib: 10,
            native_gib: 10,
            min_free_gib: 2,
            spool_gib: 64,
        }
    }
}

impl MountCacheSettings {
    /// Checks every budget is within 1..=1048576 GiB (free-space target 0..=1048576).
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

/// Reads the settings file (defaults when missing or unreadable), replaces
/// invalid encryption or cache values with defaults, and applies a
/// non-default `startup_rclone`.
pub(crate) fn load(startup_rclone: &str) -> GuiSettings {
    let mut settings = settings_path()
        .and_then(|path| read_json::<GuiSettings>(&path).ok())
        .unwrap_or_default();

    if settings.encryption.validate().is_err() {
        settings.encryption = Default::default();
    }
    if settings.mount_cache.validate().is_err() {
        settings.mount_cache = MountCacheSettings::default();
    }
    for profile in settings.mount_profiles.values_mut() {
        if profile.cache.validate().is_err() {
            profile.cache = MountCacheSettings::default();
        }
        profile.interval_seconds = profile.interval_seconds.clamp(2, 86400);
    }
    if startup_rclone != "rclone" {
        settings.rclone = startup_rclone.to_string();
    }
    settings
}

/// Validates and atomically writes the settings; returns the file path or an error message.
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

/// Path of the GUI settings file; `None` when the config directory is unknown.
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
    fn cache_preferences_persist_and_old_settings_load() {
        let old: GuiSettings = serde_json::from_str(
            r#"{"workers":7,"mount_cache":{"online_drive":false,"native_gib":10}}"#,
        )
        .unwrap();
        assert_eq!(old.mount_cache, MountCacheSettings::default());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gui-settings.json");
        let mut settings = old;
        settings.mount_cache = MountCacheSettings {
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
            let settings = GuiSettings {
                mount_cache: MountCacheSettings {
                    shard_gib: shard,
                    native_gib: native,
                    min_free_gib: free,
                    spool_gib: spool,
                },
                ..Default::default()
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
        let decoded: GuiSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(decoded.encryption, settings.encryption);
        let partial: GuiSettings =
            serde_json::from_str(r#"{"encryption":{"entropy_bits":512}}"#).unwrap();
        assert_eq!(partial.encryption.filename_encryption, "standard");
        assert_eq!(partial.encryption.entropy_bits, 512);
    }
    #[test]
    fn invalid_encryption_settings_are_rejected_before_saving() {
        let mut settings = GuiSettings::default();
        settings.encryption.entropy_bits = 12;
        assert!(save(&settings).unwrap_err().contains("entropy"));
    }
}
