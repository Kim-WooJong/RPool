//! `rpool import` implementation: validates the artifact tree, applies the
//! portable RPool settings and restores crypt secrets transactionally, rolling
//! the settings back if the crypt restore fails. Entry point [`import_package`].
use super::age_vault::{AgeDecrypt, AgeEncrypt};
use super::artifact::ArtifactPaths;
use super::crypt_restore::{preflight_crypt_vault, restore_crypt_vault};
use super::tooling::{derive_age_recipient, resolve_rclone_config};
use super::transaction::TransactionOutcome;
use crate::gui::{load_settings, save_settings, GuiSettings};
use crate::models::{PoolStore, PortableConfig, RemoteRootStore};
use crate::pool::{load_pool_store, save_pool_store};
use crate::remote_root::{load_remote_root_store, save_remote_root_store};
use crate::storage::account::limits::LimitsStore;
use crate::storage::account::store::{load_limits, save_limits};
use crate::utils::read_json;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Portable settings ready to write, with the previous local state kept for rollback.
pub(crate) struct PreparedPortableImport {
    /// Pool store before the import.
    previous_pools: PoolStore,
    /// Remote-root store before the import.
    previous_roots: RemoteRootStore,
    /// GUI settings before the import.
    previous_gui: GuiSettings,
    /// Account limits before the import.
    previous_limits: LimitsStore,
    /// Pool store from the bundle.
    next_pools: PoolStore,
    /// Remote-root store from the bundle.
    next_roots: RemoteRootStore,
    /// Local GUI settings with the portable fields replaced from the bundle.
    next_gui: GuiSettings,
    /// `None`: the bundle carries no limits; local ones stay.
    next_limits: Option<LimitsStore>,
}

impl PreparedPortableImport {
    /// Writes the new pool, remote-root, GUI and (if present) limits stores; on any
    /// failure restores the previous ones and returns the error.
    pub(crate) fn apply(&self) -> Result<()> {
        let applied = (|| -> Result<()> {
            save_pool_store(&self.next_pools)?;
            save_remote_root_store(&self.next_roots)?;
            save_settings(&self.next_gui).map_err(anyhow::Error::msg)?;
            if let Some(limits) = &self.next_limits {
                save_limits(limits)?;
            }
            Ok(())
        })();
        if let Err(error) = applied {
            let _ = self.rollback();
            return Err(error).context(
                "portable config import failed; previous rpool config was restored where possible",
            );
        }
        Ok(())
    }

    /// Writes back the previous stores (limits only if the import replaced them).
    pub(crate) fn rollback(&self) -> Result<()> {
        save_pool_store(&self.previous_pools)?;
        save_remote_root_store(&self.previous_roots)?;
        save_settings(&self.previous_gui).map_err(anyhow::Error::msg)?;
        if self.next_limits.is_some() {
            save_limits(&self.previous_limits)?;
        }
        Ok(())
    }
}

/// Validates `bundle` and snapshots the current local stores into a
/// [`PreparedPortableImport`]; nothing is written yet.
pub(crate) fn prepare_portable_import(bundle: &PortableConfig) -> Result<PreparedPortableImport> {
    super::validate_bundle(bundle)?;
    let previous_pools = load_pool_store()?;
    let previous_roots = load_remote_root_store()?;
    let previous_gui = load_settings("rclone");
    let previous_limits = load_limits()?;

    let mut next_gui = previous_gui.clone();
    if let Some(encryption) = &bundle.gui.encryption {
        next_gui.encryption = encryption.clone();
    }
    next_gui.default_remote_path = bundle.gui.default_remote_path.clone();
    next_gui.remotes = bundle.gui.remotes.clone();
    next_gui.shard_mib = bundle.gui.shard_mib;
    next_gui.workers = bundle.gui.workers;
    next_gui.retries = bundle.gui.retries;
    next_gui.data_shards = bundle.gui.data_shards;
    next_gui.parity_shards = bundle.gui.parity_shards;
    next_gui.placement = bundle.gui.placement;

    Ok(PreparedPortableImport {
        previous_pools,
        previous_roots,
        previous_gui,
        previous_limits,
        next_pools: bundle.pools.clone(),
        next_roots: bundle.remote_roots.clone(),
        next_gui,
        next_limits: bundle.account_limits.clone(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What happened to crypt secrets during an import.
pub(crate) enum PackageCryptOutcome {
    /// The bundle has no crypt remotes.
    NotPresent,
    /// The target rclone config already held exactly these secrets.
    AlreadyExact,
    /// Secrets were restored and verified (or, on a dry run, would be).
    Restored,
    /// Restored, but removing the transaction's recovery material is still pending.
    RestoredWithCleanupPending,
}

/// Result of [`import_package`], printed by `commands::config_sync::import`.
pub(crate) struct PackageImportOutcome {
    /// Canonical artifact root.
    pub(crate) artifact_root: PathBuf,
    /// Imported portable config file.
    pub(crate) portable_path: PathBuf,
    /// Number of crypt remotes in the bundle.
    pub(crate) crypt_remotes: usize,
    /// Crypt secret restore outcome.
    pub(crate) crypt: PackageCryptOutcome,
    /// Nothing was changed (validation only).
    pub(crate) dry_run: bool,
}

#[allow(clippy::too_many_arguments)] // established internal API; a params struct would only add indirection
/// Validates the artifact and its vault binding, preflights the crypt restore
/// against the target rclone.conf, then (unless `dry_run`) applies the portable
/// settings and restores crypt secrets via the config transaction. An age
/// identity outside the artifact is required when crypt remotes exist.
pub(crate) fn import_package(
    artifact_root: &Path,
    rclone: &Path,
    rclone_config: Option<&Path>,
    age: &Path,
    age_identity: Option<&Path>,
    age_recipient: Option<&str>,
    age_keygen: &Path,
    dry_run: bool,
) -> Result<PackageImportOutcome> {
    let paths = ArtifactPaths::for_import(artifact_root)?;
    let bundle: PortableConfig = read_json(&paths.portable)?;
    super::validate_bundle(&bundle)?;
    paths.validate_binding(&bundle)?;
    let prepared = prepare_portable_import(&bundle)?;

    if bundle.crypt_remotes.is_empty() {
        if !dry_run {
            prepared.apply()?;
        }
        return Ok(PackageImportOutcome {
            artifact_root: paths.root,
            portable_path: paths.portable,
            crypt_remotes: 0,
            crypt: PackageCryptOutcome::NotPresent,
            dry_run,
        });
    }

    let identity = age_identity.ok_or_else(|| {
        anyhow::anyhow!("--age-identity is required when crypt remotes are present")
    })?;
    let config = resolve_rclone_config(rclone, rclone_config)?;
    let decrypt = AgeDecrypt {
        executable: age,
        identity,
        artifact_root: &paths.root,
    };

    let already_exact = preflight_crypt_vault(rclone, &config, &bundle, &paths.vault, &decrypt)?;
    let recipient = match age_recipient {
        Some(value) if !value.is_empty() && value.trim() == value => value.to_owned(),
        Some(_) => anyhow::bail!("invalid age recipient"),
        None => derive_age_recipient(age_keygen, identity, &paths.root)?,
    };

    if dry_run {
        return Ok(PackageImportOutcome {
            artifact_root: paths.root,
            portable_path: paths.portable,
            crypt_remotes: bundle.crypt_remotes.len(),
            crypt: if already_exact {
                PackageCryptOutcome::AlreadyExact
            } else {
                PackageCryptOutcome::Restored
            },
            dry_run: true,
        });
    }

    prepared.apply()?;
    let snapshot_encrypt = AgeEncrypt {
        executable: age,
        recipient: &recipient,
    };
    let restored = restore_crypt_vault(
        rclone,
        &config,
        &bundle,
        &paths.vault,
        &decrypt,
        &snapshot_encrypt,
    );

    let transaction = match restored {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Err(rollback) = prepared.rollback() {
                return Err(error).context(format!(
                    "crypt restore failed and portable rpool settings rollback also failed: {rollback:#}"
                ));
            }
            return Err(error)
                .context("crypt restore failed; portable rpool settings were rolled back");
        }
    };

    let crypt = match transaction {
        TransactionOutcome::NoChanges => PackageCryptOutcome::AlreadyExact,
        TransactionOutcome::Committed {
            recovery_cleanup_pending: false,
        } => PackageCryptOutcome::Restored,
        TransactionOutcome::Committed {
            recovery_cleanup_pending: true,
        } => PackageCryptOutcome::RestoredWithCleanupPending,
    };

    Ok(PackageImportOutcome {
        artifact_root: paths.root,
        portable_path: paths.portable,
        crypt_remotes: bundle.crypt_remotes.len(),
        crypt,
        dry_run: false,
    })
}
