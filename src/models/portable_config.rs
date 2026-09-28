use super::{Placement, PoolStore, RemoteRootStore};
use serde::{Deserialize, Serialize};

pub(crate) const CONFIG_SYNC_FORMAT: &str = "rpool-portable-config";
pub(crate) const CONFIG_SYNC_VERSION: u32 = 1;
pub(crate) const SECRET_VAULT_FORMAT: &str = "age";
pub(crate) const SECRET_VAULT_PATH: &str = "secrets/rclone.age";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PortableGuiSettings {
    pub(crate) default_remote_path: String,
    pub(crate) remotes: Vec<String>,
    pub(crate) shard_mib: u64,
    pub(crate) workers: usize,
    pub(crate) retries: u32,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
    pub(crate) placement: Placement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableConfig {
    pub(crate) format: String,
    pub(crate) version: u32,
    pub(crate) exported_at_unix: u64,
    pub(crate) pools: PoolStore,
    pub(crate) remote_roots: RemoteRootStore,
    pub(crate) gui: PortableGuiSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) secret_vault: Option<PortableSecretVault>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) crypt_remotes: Vec<PortableCryptRemote>,
}

/// Explicit allowlist: credentials can never be added via an arbitrary map.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortableSecretVault {
    pub(crate) format: String,
    pub(crate) path: String,
    pub(crate) blake3: String,
}

impl PortableSecretVault {
    pub(crate) fn new(blake3: String) -> anyhow::Result<Self> {
        let value = Self {
            format: SECRET_VAULT_FORMAT.to_string(),
            path: SECRET_VAULT_PATH.to_string(),
            blake3,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.format != SECRET_VAULT_FORMAT || self.path != SECRET_VAULT_PATH
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
pub(crate) struct PortableCryptRemote {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) remote: String,
    pub(crate) filename_encryption: String,
    pub(crate) directory_name_encryption: bool,
}

impl PortableCryptRemote {
    pub(crate) fn validate_structure(&self) -> anyhow::Result<()> {
        super::secrets::validate_remote_name(&self.name)?;
        if self.kind != "crypt" || !matches!(self.filename_encryption.as_str(), "standard" | "obfuscate" | "off") {
            anyhow::bail!("unsupported portable crypt definition");
        }
        if self.remote.chars().any(char::is_control) {
            anyhow::bail!("invalid portable crypt backing reference");
        }
        // Named remotes only. In-line :backend,key=credential: connection
        // strings would turn supposedly portable metadata into a secret store.
        let (backing, _) = self.remote.split_once(':')
            .ok_or_else(|| anyhow::anyhow!("portable crypt backing must reference a named remote"))?;
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
