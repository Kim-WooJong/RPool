//! Fail-closed crypt restoration with encrypted recovery snapshots.
//!
//! Encrypted rclone configs are updated through an encrypted staging file.
//! Plaintext configs are patched only in memory and committed directly to the
//! already-plaintext live file, so no additional plaintext temp/backup file is
//! created by rpool.
/// Recovery directory layout, journal, config lock and commit primitives.
pub(super) mod files;
/// Explicit recovery of an interrupted transaction from its journal/snapshot.
mod recovery;
/// The transaction state machine (`run`): preflight, snapshot, stage, commit, rollback.
mod run;
#[cfg(test)]
mod tests;

use super::age_vault::{AgeDecrypt, AgeEncrypt};
use crate::models::sensitive::SensitiveBytes;
use anyhow::Result;
use std::path::Path;

#[cfg(test)]
pub(crate) use recovery::recover_interrupted;
pub(crate) use run::run;

/// Result of a successful `run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionOutcome {
    /// Preflight found every requested value already exact; nothing was written.
    NoChanges,
    /// The new config was committed and verified.
    /// `recovery_cleanup_pending` is true when the recovery directory could not be removed.
    Committed {
        /// True when the recovery directory could not be removed.
        recovery_cleanup_pending: bool,
    },
}

/// Result of `recovery::recover_interrupted`. Each variant's `cleanup_pending`
/// is true when the recovery directory could not be removed afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryOutcome {
    /// The live config already matches the journaled original; nothing replaced.
    OriginalAlreadyPresent {
        /// True when the recovery directory could not be removed.
        cleanup_pending: bool,
    },
    /// The original was decrypted from the snapshot and written back, then verified.
    OriginalRestored {
        /// True when the recovery directory could not be removed.
        cleanup_pending: bool,
    },
    /// The commit had reached `Phase::Verified`, so the new config is kept.
    VerifiedCommitKept {
        /// True when the recovery directory could not be removed.
        cleanup_pending: bool,
    },
}

/// Backend that knows how to validate, patch and verify one rclone config for
/// a transaction. Implemented by `crypt_restore::RcloneDriver` (and a test mock).
pub(crate) trait ConfigDriver {
    /// Validate every target and return true only when every requested obscured
    /// value is already exact. No mutation is permitted here.
    fn preflight(&self, config: &Path) -> Result<bool>;

    /// Encrypted-config path: rclone operates on an encrypted staging file.
    fn apply_to_encrypted_stage(&self, config: &Path) -> Result<()>;

    /// Plaintext-config path: build the entire candidate in RAM. Implementations
    /// must not create files or invoke a writer which persists plaintext copies.
    fn build_plaintext_candidate(&self, original: &[u8]) -> Result<SensitiveBytes>;

    /// Verify an on-disk config through the authoritative rclone reader.
    fn verify(&self, config: &Path) -> Result<()>;
}

/// Implementations must encrypt snapshots. The production implementation is age;
/// test implementations use explicit fake ciphertext fixtures, never credentials.
pub(crate) trait SnapshotStore {
    /// Encrypt `config_bytes` into the snapshot file at `output`.
    fn seal(&self, output: &Path, config_bytes: &[u8]) -> Result<()>;
    /// Decrypt the snapshot at `input`; used for round-trip checks and recovery.
    fn open(&self, input: &Path) -> Result<SensitiveBytes>;
}

/// Production `SnapshotStore` backed by the age vault; built by
/// `crypt_restore` before calling `transaction::run`.
pub(crate) struct AgeSnapshot<'a, 'b> {
    /// Encrypts snapshots to the vault recipient.
    pub(crate) encrypt: &'a AgeEncrypt<'b>,
    /// Decrypts snapshots with the vault identity.
    pub(crate) decrypt: &'a AgeDecrypt<'b>,
}

impl SnapshotStore for AgeSnapshot<'_, '_> {
    fn seal(&self, output: &Path, bytes: &[u8]) -> Result<()> {
        self.encrypt.write_snapshot(output, bytes)
    }
    fn open(&self, input: &Path) -> Result<SensitiveBytes> {
        self.decrypt.read_bytes(input)
    }
}
