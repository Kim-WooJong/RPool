//! Transaction runner for crypt-config restoration.
//!
//! `run` locks the config, preflights the driver, writes an age-encrypted
//! snapshot plus journal, builds the candidate (encrypted stage or in-RAM
//! plaintext), commits, verifies, and rolls back on failure. Called by
//! `config_sync::crypt_restore`.
use super::files::{
    self, Committer, ConfigCommit, ConfigFormat, ConfigLock, Journal, Phase, SNAPSHOT, STAGE,
};
use super::{ConfigDriver, SnapshotStore, TransactionOutcome};
use crate::config_sync::age_vault::sync_directory;
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::Path;

/// Run a restore transaction against `config` using the production
/// `ConfigCommit` writer. Returns `NoChanges` when preflight reports the
/// config is already exact; errors leave the original (or a retained
/// encrypted recovery) in place.
pub(crate) fn run(
    config: &Path,
    driver: &impl ConfigDriver,
    snapshots: &impl SnapshotStore,
) -> Result<TransactionOutcome> {
    run_with(config, driver, snapshots, &ConfigCommit)
}

/// `run` with an injectable `Committer`, so tests can simulate write failures.
pub(super) fn run_with(
    config: &Path,
    driver: &impl ConfigDriver,
    snapshots: &impl SnapshotStore,
    committer: &impl Committer,
) -> Result<TransactionOutcome> {
    let target = files::target_path(config)?;
    let _lock = ConfigLock::acquire(&target)?;
    let recovery = files::recovery_path(&target)?;
    if files::exists(&recovery)? {
        bail!("pending rpool recovery must be resolved before another restore");
    }
    let metadata = files::regular_file(&target)?;
    if metadata.permissions().readonly() {
        bail!("configuration is read-only; restore refused");
    }
    let permissions = metadata.permissions();
    let original = files::read_config(&target)?;
    let format = files::format(&original.0)?;

    let already_exact = driver.preflight(&target)?;
    if files::read_config(&target)?.0 != original.0 {
        bail!("configuration changed during preflight; nothing was written");
    }
    if already_exact {
        return Ok(TransactionOutcome::NoChanges);
    }

    files::create_recovery(&recovery)?;
    let mut journal = Journal::new(&original.0, format);
    let prepared = (|| -> Result<_> {
        journal.write(&recovery)?;
        let snapshot = recovery.join(SNAPSHOT);
        snapshots.seal(&snapshot, &original.0)?;
        // A recipient typo/wrong identity must fail BEFORE config mutation.
        let roundtrip = snapshots.open(&snapshot)?;
        if roundtrip.0 != original.0 {
            bail!("encrypted snapshot round-trip verification failed");
        }

        let candidate = match format {
            ConfigFormat::Encrypted => {
                let stage = recovery.join(STAGE);
                files::stage_ciphertext(&stage, &original.0)?;
                driver.apply_to_encrypted_stage(&stage)?;
                driver.verify(&stage)?;
                let candidate = files::read_config(&stage)?;
                if files::format(&candidate.0)? != ConfigFormat::Encrypted {
                    bail!("encrypted staging config changed representation");
                }
                fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&stage)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| anyhow!("cannot synchronize staging config"))?;
                candidate
            }
            ConfigFormat::Plaintext => {
                // The candidate exists only in SensitiveBytes. No plaintext
                // staging/backup path is ever created by this branch.
                let candidate = driver.build_plaintext_candidate(&original.0)?;
                if files::format(&candidate.0)? != ConfigFormat::Plaintext {
                    bail!("plaintext candidate changed representation");
                }
                candidate
            }
        };
        if candidate.0 == original.0 {
            bail!("restore candidate unexpectedly contains no changes");
        }
        journal.candidate_digest = Some(files::digest(&candidate.0));
        journal.phase = Phase::Ready;
        journal.write(&recovery)?;
        Ok(candidate)
    })();
    let candidate = match prepared {
        Ok(value) => value,
        Err(_) => {
            let cleaned = files::cleanup_recovery(&recovery);
            if cleaned {
                bail!("restore preparation failed; live configuration was not changed");
            }
            bail!("restore preparation failed; live configuration was not changed; recovery cleanup is pending");
        }
    };

    // The sidecar lock is cooperative. Detect non-rpool edits immediately before
    // mutation and refuse to overwrite them.
    if files::read_config(&target)?.0 != original.0 {
        bail!(
            "external configuration change detected; not overwritten; encrypted recovery retained"
        );
    }
    journal.phase = Phase::Committing;
    journal.write(&recovery)?;
    if files::read_config(&target)?.0 != original.0 {
        bail!("external configuration change detected before commit; encrypted recovery retained");
    }

    let mut plaintext_write_recovery_failed = false;
    let committed = (|| -> Result<()> {
        if let Err(_write_error) = committer.replace(&target, &candidate.0, &permissions, format) {
            // Plaintext direct-write can fail after truncation. Make one immediate
            // restoration attempt from the RAM copy. Only call it recovered if
            // the bytes and durability sync can both be verified.
            if format == ConfigFormat::Plaintext {
                let _ = committer.replace(&target, &original.0, &permissions, format);
                let bytes_ok = files::read_config(&target)
                    .map(|value| value.0 == original.0)
                    .unwrap_or(false);
                let file_sync_ok = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&target)
                    .and_then(|file| file.sync_all())
                    .is_ok();
                let dir_sync_ok = target
                    .parent()
                    .map(|parent| sync_directory(parent).is_ok())
                    .unwrap_or(false);
                plaintext_write_recovery_failed = !(bytes_ok && file_sync_ok && dir_sync_ok);
            }
            bail!("configuration commit failed");
        }
        sync_directory(
            target
                .parent()
                .ok_or_else(|| anyhow!("configuration parent missing"))?,
        )?;
        driver.verify(&target)?;
        if files::read_config(&target)?.0 != candidate.0 {
            bail!("configuration changed during post-commit verification");
        }
        journal.phase = Phase::Verified;
        journal.write(&recovery)?;
        Ok(())
    })();
    if committed.is_ok() {
        return Ok(TransactionOutcome::Committed {
            recovery_cleanup_pending: !files::cleanup_recovery(&recovery),
        });
    }
    if plaintext_write_recovery_failed {
        bail!("restore commit failed; plaintext rollback durability could not be verified; encrypted recovery retained; further restore is blocked");
    }

    // Only revert a configuration we can prove is still ours. If a plaintext
    // write failed partially and the immediate restoration was not durable, the
    // encrypted recovery is retained above instead of claiming success.
    let rollback = (|| -> Result<()> {
        let current = files::read_config(&target)?;
        if current.0 == original.0 {
            return Ok(());
        }
        if current.0 != candidate.0 {
            bail!("configuration no longer belongs to this transaction");
        }
        committer.replace(&target, &original.0, &permissions, format)?;
        sync_directory(
            target
                .parent()
                .ok_or_else(|| anyhow!("configuration parent missing"))?,
        )?;
        if files::read_config(&target)?.0 != original.0 {
            bail!("rollback verification failed");
        }
        Ok(())
    })();
    if rollback.is_ok() {
        let cleaned = files::cleanup_recovery(&recovery);
        if cleaned {
            bail!("restore failed; original configuration was restored and verified byte-for-byte");
        }
        bail!("restore failed; original configuration was restored and verified; recovery cleanup is pending");
    }
    bail!("restore failed; automatic rollback could not be verified; encrypted recovery retained; further restore is blocked")
}
