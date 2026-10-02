//! Portable configuration bundle ([`PortableConfig`]) moved between PCs by
//! `rpool export`/`import` (`config_sync`): pools, remote roots, GUI
//! defaults, crypt remote definitions and a reference to the age-encrypted
//! secret vault. Credentials are never stored in the JSON itself.
use super::{Placement, PoolStore, RemoteRootStore};
use serde::{Deserialize, Serialize};

/// `PortableConfig::format` marker of a bundle.
pub(crate) const CONFIG_SYNC_FORMAT: &str = "rpool-portable-config";
/// Current bundle version.
pub(crate) const CONFIG_SYNC_VERSION: u32 = 1;
/// Only supported secret vault format (age encryption).
pub(crate) const SECRET_VAULT_FORMAT: &str = "age";
/// Fixed path of the secret vault inside the bundle; any other is rejected.
pub(crate) const SECRET_VAULT_PATH: &str = "secrets/rclone.age";

#[derive(Debug, Clone, Serialize, Deserialize)]
/// GUI defaults carried in a bundle (preferences only, no secrets).
pub(crate) struct PortableGuiSettings {
    // Preferences only, never crypt passwords. Older bundles leave local defaults intact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Default encryption options for new crypt remotes; `None` in older bundles.
    pub(crate) encryption: Option<crate::config_sync::provision::EncryptionDefaults>,
    /// Default remote folder for new pools/crypt remotes.
    pub(crate) default_remote_path: String,
    /// Default remote list of the GUI.
    pub(crate) remotes: Vec<String>,
    /// Default shard size, MiB.
    pub(crate) shard_mib: u64,
    /// Default parallel transfers.
    pub(crate) workers: usize,
    /// Default retries per transfer.
    pub(crate) retries: u32,
    /// Default data shards (K).
    pub(crate) data_shards: usize,
    /// Default parity shards (M).
    pub(crate) parity_shards: usize,
    /// Default placement.
    pub(crate) placement: Placement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// The exported bundle, built by `config_sync::export` and validated on
/// import (unknown fields are rejected).
pub(crate) struct PortableConfig {
    /// Must be [`CONFIG_SYNC_FORMAT`].
    pub(crate) format: String,
    /// Bundle version ([`CONFIG_SYNC_VERSION`]).
    pub(crate) version: u32,
    /// Export time, Unix seconds.
    pub(crate) exported_at_unix: u64,
    /// All pool definitions and drive settings.
    pub(crate) pools: PoolStore,
    /// Remote root paths.
    pub(crate) remote_roots: RemoteRootStore,
    /// GUI defaults.
    pub(crate) gui: PortableGuiSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Binding of the age-encrypted rclone secrets; `None` when not exported.
    pub(crate) secret_vault: Option<PortableSecretVault>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Crypt remote definitions (without passwords), recreated on import.
    pub(crate) crypt_remotes: Vec<PortableCryptRemote>,
    /// Per-account limits and the bandwidth timetable; absent when defaults
    /// (older bundles import without touching local limits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) account_limits: Option<crate::storage::account::limits::LimitsStore>,
}

/// Explicit allowlist: credentials can never be added via an arbitrary map.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableSecretVault {
    /// Vault format, must be [`SECRET_VAULT_FORMAT`].
    pub(crate) format: String,
    /// Vault path in the bundle, must be [`SECRET_VAULT_PATH`].
    pub(crate) path: String,
    /// BLAKE3 hex (64 chars) of the vault file, checked on import.
    pub(crate) blake3: String,
}

impl PortableSecretVault {
    /// Binding for a vault with digest `blake3`; fails on an invalid digest.
    pub(crate) fn new(blake3: String) -> anyhow::Result<Self> {
        let value = Self {
            format: SECRET_VAULT_FORMAT.to_string(),
            path: SECRET_VAULT_PATH.to_string(),
            blake3,
        };
        value.validate()?;
        Ok(value)
    }

    /// Rejects any format, path or digest other than the fixed ones.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.format != SECRET_VAULT_FORMAT
            || self.path != SECRET_VAULT_PATH
            || self.blake3.len() != 64
            || !self.blake3.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            anyhow::bail!("invalid crypt secret vault binding");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Non-secret definition of one rclone crypt remote.
pub(crate) struct PortableCryptRemote {
    /// Crypt remote name.
    pub(crate) name: String,
    #[serde(rename = "type")]
    /// rclone backend type; must be `crypt`.
    pub(crate) kind: String,
    /// Backing remote (`name:path`); must reference a named remote.
    pub(crate) remote: String,
    /// rclone `filename_encryption`: `standard`, `obfuscate` or `off`.
    pub(crate) filename_encryption: String,
    /// rclone `directory_name_encryption`.
    pub(crate) directory_name_encryption: bool,
    /// rclone `filename_encoding`; omitted for the default `base32`, so
    /// packages without it stay readable by older RPool versions.
    #[serde(
        default = "default_encoding",
        skip_serializing_if = "is_default_encoding"
    )]
    pub(crate) filename_encoding: String,
}

/// Serde default of `filename_encoding` (`base32`).
fn default_encoding() -> String {
    "base32".into()
}
/// True for `base32`, which is then omitted from JSON.
fn is_default_encoding(value: &String) -> bool {
    value == "base32"
}

impl PortableCryptRemote {
    /// Checks names, type, encryption options and that the backing is a named
    /// remote (inline connection strings could carry credentials).
    pub(crate) fn validate_structure(&self) -> anyhow::Result<()> {
        super::secrets::validate_remote_name(&self.name)?;
        if self.kind != "crypt"
            || !matches!(
                self.filename_encryption.as_str(),
                "standard" | "obfuscate" | "off"
            )
        {
            anyhow::bail!("unsupported portable crypt definition");
        }
        crate::config_sync::provision::validate_filename_encoding(&self.filename_encoding)?;
        if self.remote.chars().any(char::is_control) {
            anyhow::bail!("invalid portable crypt backing reference");
        }
        // Named remotes only. In-line :backend,key=credential: connection
        // strings would turn supposedly portable metadata into a secret store.
        let (backing, _) = self.remote.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("portable crypt backing must reference a named remote")
        })?;
        super::secrets::validate_remote_name(backing)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_binding_uses_fixed_age_path() {
        let binding = PortableSecretVault::new("a".repeat(64)).unwrap();
        assert_eq!(binding.format, SECRET_VAULT_FORMAT);
        assert_eq!(binding.path, SECRET_VAULT_PATH);
    }

    #[test]
    fn vault_binding_rejects_path_substitution() {
        let binding = PortableSecretVault {
            format: SECRET_VAULT_FORMAT.into(),
            path: "../identity.agekey".into(),
            blake3: "b".repeat(64),
        };
        assert!(binding.validate().is_err());
    }

    #[test]
    fn vault_binding_rejects_invalid_digest() {
        assert!(PortableSecretVault::new("not-a-digest".into()).is_err());
    }
}
