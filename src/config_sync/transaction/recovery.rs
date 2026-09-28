use super::files::{self, Committer, ConfigCommit, ConfigLock, Journal, Phase, SNAPSHOT};
use super::{RecoveryOutcome, SnapshotStore};
use crate::config_sync::age_vault::sync_directory;
use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// Explicit recovery only. Startup/import must not silently roll back a later
/// independent configuration edit. No paths are taken from the journal.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Explicit recovery remains opt-in; startup and import must not automatically roll back config"
    )
)]
pub(crate) fn recover_interrupted(
    config: &Path,
    snapshots: &impl SnapshotStore,
) -> Result<RecoveryOutcome> {
    let target = files::target_path(config)?;
    let _lock = ConfigLock::acquire(&target)?;
    let recovery = files::recovery_path(&target)?;
    let journal = Journal::read(&recovery)?;
    let current = files::read_config(&target)?;
    let current_digest = files::digest(&current.0);

    if current_digest == journal.original_digest {
        return Ok(RecoveryOutcome::OriginalAlreadyPresent {
            cleanup_pending: !files::cleanup_recovery(&recovery),
        });
    }
    if journal.candidate_digest.as_deref() != Some(current_digest.as_str()) {
        bail!("recovery conflict: current config matches neither original nor candidate; no files were replaced");
    }
    if journal.phase == Phase::Verified {
        return Ok(RecoveryOutcome::VerifiedCommitKept {
            cleanup_pending: !files::cleanup_recovery(&recovery),
        });
    }

    let original = snapshots.open(&recovery.join(SNAPSHOT))?;
    if files::format(&original.0)? != journal.format
        || files::digest(&original.0) != journal.original_digest
    {
        bail!("recovery snapshot validation failed; current configuration was not changed");
    }
    // Recheck after the potentially slow age operation.
    if files::read_config(&target)?.0 != current.0 {
        bail!("configuration changed during recovery; no replacement was attempted");
    }
    let permissions = files::regular_file(&target)?.permissions();
    ConfigCommit.replace(&target, &original.0, &permissions, journal.format)?;
    sync_directory(
        target
            .parent()
            .ok_or_else(|| anyhow!("configuration parent missing"))?,
    )?;
    if files::read_config(&target)?.0 != original.0 {
        bail!("recovery replacement could not be verified; encrypted recovery retained");
    }
    Ok(RecoveryOutcome::OriginalRestored {
        cleanup_pending: !files::cleanup_recovery(&recovery),
    })
}
