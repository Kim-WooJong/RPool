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

pub(crate) struct PreparedPortableImport {
    previous_pools: PoolStore,
    previous_roots: RemoteRootStore,
    previous_gui: GuiSettings,
    previous_limits: LimitsStore,
    next_pools: PoolStore,
    next_roots: RemoteRootStore,
    next_gui: GuiSettings,
    /// `None`: the bundle carries no limits; local ones stay.
    next_limits: Option<LimitsStore>,
}

impl PreparedPortableImport {
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

/// Legacy portable-only import retained for compatibility. A crypt-aware bundle
/// must use the B7 package importer so secrets can never be silently skipped.
pub(crate) fn import_bundle(input: &Path, dry_run: bool) -> Result<()> {
    let bundle: PortableConfig = read_json(input)?;
    super::validate_bundle(&bundle)?;

    if !bundle.crypt_remotes.is_empty() || bundle.secret_vault.is_some() {
        anyhow::bail!("crypt-aware import requires `rpool import`; no settings were changed");
    }

    let prepared = prepare_portable_import(&bundle)?;
    if dry_run {
        return Ok(());
    }
    prepared.apply()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageCryptOutcome {
    NotPresent,
    AlreadyExact,
    Restored,
    RestoredWithCleanupPending,
}

pub(crate) struct PackageImportOutcome {
    pub(crate) artifact_root: PathBuf,
    pub(crate) portable_path: PathBuf,
    pub(crate) crypt_remotes: usize,
    pub(crate) crypt: PackageCryptOutcome,
    pub(crate) dry_run: bool,
}

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
