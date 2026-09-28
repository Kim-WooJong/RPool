//! Fail-closed crypt restoration with encrypted recovery snapshots.
//!
//! Encrypted rclone configs are updated through an encrypted staging file.
//! Plaintext configs are patched only in memory and committed directly to the
//! already-plaintext live file, so no additional plaintext temp/backup file is
//! created by rpool.
pub(super) mod files;
mod recovery;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionOutcome {
    NoChanges,
    Committed { recovery_cleanup_pending: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryOutcome {
    OriginalAlreadyPresent { cleanup_pending: bool },
    OriginalRestored { cleanup_pending: bool },
    VerifiedCommitKept { cleanup_pending: bool },
}

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
    fn seal(&self, output: &Path, config_bytes: &[u8]) -> Result<()>;
    fn open(&self, input: &Path) -> Result<SensitiveBytes>;
}

pub(crate) struct AgeSnapshot<'a, 'b> {
    pub(crate) encrypt: &'a AgeEncrypt<'b>,
    pub(crate) decrypt: &'a AgeDecrypt<'b>,
}

impl SnapshotStore for AgeSnapshot<'_, '_> {
    fn seal(&self, output: &Path, bytes: &[u8]) -> Result<()> { self.encrypt.write_snapshot(output, bytes) }
    fn open(&self, input: &Path) -> Result<SensitiveBytes> { self.decrypt.read_bytes(input) }
}
