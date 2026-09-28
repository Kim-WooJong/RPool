use super::age_vault::AgeEncrypt;
use super::artifact::ArtifactPaths;
use super::crypt_secrets::extract_crypt_secrets;
use super::tooling::resolve_rclone_config;
use crate::gui::load_settings;
use crate::models::{
    PortableConfig, PortableCryptRemote, PortableGuiSettings, PortableSecretVault,
    CONFIG_SYNC_FORMAT, CONFIG_SYNC_VERSION,
};
use crate::pool::load_pool_store;
use crate::remote_root::load_remote_root_store;
use crate::utils::{now_unix, save_json_atomic};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub(crate) struct PackageExportOutcome {
    pub(crate) artifact_root: PathBuf,
    pub(crate) portable_path: PathBuf,
    pub(crate) vault_path: Option<PathBuf>,
    pub(crate) crypt_remotes: usize,
}

fn build_bundle(
    crypt_remotes: Vec<PortableCryptRemote>,
    secret_vault: Option<PortableSecretVault>,
) -> Result<PortableConfig> {
    let gui = load_settings("rclone");
    let bundle = PortableConfig {
        format: CONFIG_SYNC_FORMAT.to_string(),
        version: CONFIG_SYNC_VERSION,
        exported_at_unix: now_unix(),
        pools: load_pool_store()?,
        remote_roots: load_remote_root_store()?,
        crypt_remotes,
        secret_vault,
        gui: PortableGuiSettings {
            default_remote_path: gui.default_remote_path,
            remotes: gui.remotes,
            shard_mib: gui.shard_mib,
            workers: gui.workers,
            retries: gui.retries,
            data_shards: gui.data_shards,
            parity_shards: gui.parity_shards,
            placement: gui.placement,
        },
    };
    super::validate_bundle(&bundle)?;
    Ok(bundle)
}

/// Legacy portable-only export retained for existing dot workflows. Crypt
/// definitions and credentials are intentionally absent from this JSON-only path.
pub(crate) fn export_bundle(output: &Path) -> Result<PathBuf> {
    let bundle = build_bundle(Vec::new(), None)?;
    save_json_atomic(output, &bundle)?;
    Ok(output.to_path_buf())
}

/// B7 package export. The portable JSON never contains crypt passwords. If
/// crypt remotes exist, their already-obscured values are encrypted directly
/// into `secrets/rclone.age` and the JSON binds to that ciphertext by digest.
pub(crate) fn export_package(
    artifact_root: &Path,
    rclone: &Path,
    rclone_config: Option<&Path>,
    age: &Path,
    age_recipient: Option<&str>,
) -> Result<PackageExportOutcome> {
    let paths = ArtifactPaths::for_export(artifact_root)?;
    let config = resolve_rclone_config(rclone, rclone_config)?;
    let (secrets, crypt_remotes) = extract_crypt_secrets(rclone, &config)?;

    if crypt_remotes.is_empty() {
        let bundle = build_bundle(Vec::new(), None)?;
        save_json_atomic(&paths.portable, &bundle)?;
        paths.remove_stale_vault()?;
        return Ok(PackageExportOutcome {
            artifact_root: paths.root,
            portable_path: paths.portable,
            vault_path: None,
            crypt_remotes: 0,
        });
    }

    let recipient = age_recipient
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--age-recipient is required when crypt remotes are present"))?;
    if recipient.trim() != recipient {
        bail!("invalid age recipient");
    }

    let encrypt = AgeEncrypt { executable: age, recipient };
    encrypt.write_bundle(&paths.vault, &secrets)?;
    let binding = paths.binding()?;
    let crypt_count = crypt_remotes.len();
    let bundle = build_bundle(crypt_remotes, Some(binding))?;
    save_json_atomic(&paths.portable, &bundle)?;
    paths.validate_binding(&bundle)?;

    Ok(PackageExportOutcome {
        artifact_root: paths.root,
        portable_path: paths.portable,
        vault_path: Some(paths.vault),
        crypt_remotes: crypt_count,
    })
}
