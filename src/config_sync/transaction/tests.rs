//! State-machine/file-operation tests with explicitly fake encrypted fixtures.
//! These do NOT test age cryptography or real rclone; B6 supplies that coverage.
use super::files::{self, Committer, ConfigCommit, ConfigFormat, Journal, Phase, SNAPSHOT, STAGE};
use super::*;
use anyhow::{bail, Result};
use crate::models::sensitive::SensitiveBytes;
use std::cell::Cell;
use std::fs::{self, Permissions};
use std::path::{Path, PathBuf};

const ORIGINAL: &[u8] = b"# test fixture, not a usable credential\nRCLONE_ENCRYPT_V0:\nORIGINAL-MOCK-CIPHERTEXT\n";
const CANDIDATE: &[u8] = b"RCLONE_ENCRYPT_V0:\nCANDIDATE-MOCK-CIPHERTEXT\n";
const EXTERNAL: &[u8] = b"RCLONE_ENCRYPT_V0:\nEXTERNAL-MOCK-CIPHERTEXT\n";
const PLAIN_ORIGINAL: &[u8] = b"[vault]\ntype = crypt\npassword = OLDOLDOLDOLDOLDOLDOLD12\n";
const PLAIN_CANDIDATE: &[u8] = b"[vault]\ntype = crypt\npassword = AAAAAAAAAAAAAAAAAAAAAAA\n";

struct MockVault { fail_open: bool, corrupt: bool }
impl MockVault { fn working() -> Self { Self { fail_open: false, corrupt: false } } }
impl SnapshotStore for MockVault {
    fn seal(&self, output: &Path, ciphertext: &[u8]) -> Result<()> {
        // Test fixture wrapper only, NEVER a production SnapshotStore.
        let mut bytes = b"MOCK-AGE\n".to_vec();
        bytes.extend_from_slice(ciphertext);
        fs::write(output, bytes)?;
        Ok(())
    }
    fn open(&self, input: &Path) -> Result<SensitiveBytes> {
        if self.fail_open { bail!("mock identity failure"); }
        let bytes = fs::read(input)?;
        if !bytes.starts_with(b"MOCK-AGE\n") { bail!("mock corrupted snapshot"); }
        let mut output = bytes[9..].to_vec();
        if self.corrupt { output.push(b'!'); }
        Ok(SensitiveBytes(output))
    }
}

struct MockDriver {
    target: PathBuf,
    fail_preflight: bool,
    fail_stage: bool,
    fail_live_verify: bool,
    external_change: bool,
    calls: Cell<usize>,
}
impl MockDriver {
    fn new(target: &Path) -> Self {
        Self { target: target.to_owned(), fail_preflight: false, fail_stage: false,
            fail_live_verify: false, external_change: false, calls: Cell::new(0) }
    }
}
impl ConfigDriver for MockDriver {
    fn preflight(&self, config: &Path) -> Result<bool> {
        if self.fail_preflight { bail!("mock invalid target"); }
        let bytes = fs::read(config)?;
        Ok(bytes == CANDIDATE || bytes == PLAIN_CANDIDATE)
    }
    fn apply_to_encrypted_stage(&self, config: &Path) -> Result<()> {
        assert!(config != self.target.as_path());
        self.calls.set(self.calls.get() + 1);
        fs::write(config, CANDIDATE)?;
        if self.external_change { fs::write(&self.target, EXTERNAL)?; }
        if self.fail_stage { bail!("mock second remote update failure"); }
        Ok(())
    }
    fn build_plaintext_candidate(&self, original: &[u8]) -> Result<SensitiveBytes> {
        self.calls.set(self.calls.get() + 1);
        if self.external_change { fs::write(&self.target, b"[external]\ntype = local\n")?; }
        if self.fail_stage { bail!("mock plaintext candidate failure"); }
        if original != PLAIN_ORIGINAL { bail!("unexpected plaintext fixture"); }
        Ok(SensitiveBytes(PLAIN_CANDIDATE.to_vec()))
    }
    fn verify(&self, config: &Path) -> Result<()> {
        if self.fail_live_verify && config == self.target.as_path() { bail!("mock post-commit verification failure"); }
        let bytes = fs::read(config)?;
        if bytes != CANDIDATE && bytes != PLAIN_CANDIDATE { bail!("mock exact value mismatch"); }
        Ok(())
    }
}

struct FailSecondReplace(Cell<usize>);
impl Committer for FailSecondReplace {
    fn replace(&self, target: &Path, bytes: &[u8], permissions: &Permissions, format: ConfigFormat) -> Result<()> {
        self.0.set(self.0.get() + 1);
        if self.0.get() == 2 { bail!("mock rollback write failure"); }
        ConfigCommit.replace(target, bytes, permissions, format)
    }
}

fn setup() -> Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("rclone.conf");
    fs::write(&target, ORIGINAL)?;
    Ok((dir, target.canonicalize()?))
}
fn pending(target: &Path) -> Result<PathBuf> { files::recovery_path(target) }

#[test]
fn successful_transaction_commits_verified_candidate() -> Result<()> {
    let (_dir, target) = setup()?;
    let result = run(&target, &MockDriver::new(&target), &MockVault::working())?;
    assert!(matches!(result, TransactionOutcome::Committed { recovery_cleanup_pending: false }));
    assert!(fs::read(&target)? == CANDIDATE);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn repeated_restore_is_noop_without_snapshot_or_update() -> Result<()> {
    let (_dir, target) = setup()?;
    let driver = MockDriver::new(&target);
    run(&target, &driver, &MockVault::working())?;
    assert!(matches!(run(&target, &driver, &MockVault::working())?, TransactionOutcome::NoChanges));
    assert_eq!(driver.calls.get(), 1);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn plaintext_config_commits_without_plaintext_stage_file() -> Result<()> {
    let (_dir, target) = setup()?;
    fs::write(&target, PLAIN_ORIGINAL)?;
    let driver = MockDriver::new(&target);
    let result = run(&target, &driver, &MockVault::working())?;
    assert!(matches!(result, TransactionOutcome::Committed { recovery_cleanup_pending: false }));
    assert!(fs::read(&target)? == PLAIN_CANDIDATE);
    assert_eq!(driver.calls.get(), 1);
    assert!(!pending(&target)?.join(STAGE).exists());
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn plaintext_postcommit_failure_restores_original_exactly() -> Result<()> {
    let (_dir, target) = setup()?;
    fs::write(&target, PLAIN_ORIGINAL)?;
    let mut driver = MockDriver::new(&target);
    driver.fail_live_verify = true;
    assert!(run(&target, &driver, &MockVault::working()).is_err());
    assert!(fs::read(&target)? == PLAIN_ORIGINAL);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn invalid_target_preflight_leaves_original_and_no_recovery_dir() -> Result<()> {
    let (_dir, target) = setup()?;
    let mut driver = MockDriver::new(&target);
    driver.fail_preflight = true;
    assert!(run(&target, &driver, &MockVault::working()).is_err());
    assert!(fs::read(&target)? == ORIGINAL);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn wrong_snapshot_identity_fails_before_any_update() -> Result<()> {
    let (_dir, target) = setup()?;
    let driver = MockDriver::new(&target);
    assert!(run(&target, &driver, &MockVault { fail_open: true, corrupt: false }).is_err());
    assert!(fs::read(&target)? == ORIGINAL);
    assert_eq!(driver.calls.get(), 0);
    Ok(())
}

#[test]
fn corrupted_snapshot_roundtrip_fails_before_any_update() -> Result<()> {
    let (_dir, target) = setup()?;
    let driver = MockDriver::new(&target);
    assert!(run(&target, &driver, &MockVault { fail_open: false, corrupt: true }).is_err());
    assert!(fs::read(&target)? == ORIGINAL);
    assert_eq!(driver.calls.get(), 0);
    Ok(())
}

#[test]
fn partial_stage_update_failure_preserves_original_bytes() -> Result<()> {
    let (_dir, target) = setup()?;
    let mut driver = MockDriver::new(&target);
    driver.fail_stage = true;
    assert!(run(&target, &driver, &MockVault::working()).is_err());
    assert!(fs::read(&target)? == ORIGINAL);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn postcommit_verification_failure_rolls_back_exactly() -> Result<()> {
    let (_dir, target) = setup()?;
    let mut driver = MockDriver::new(&target);
    driver.fail_live_verify = true;
    let result = run(&target, &driver, &MockVault::working());
    assert!(result.is_err());
    assert!(fs::read(&target)? == ORIGINAL);
    assert!(!pending(&target)?.exists());
    Ok(())
}

#[test]
fn failed_rollback_keeps_recovery_and_blocks_another_restore() -> Result<()> {
    let (_dir, target) = setup()?;
    let mut driver = MockDriver::new(&target);
    driver.fail_live_verify = true;
    let committer = FailSecondReplace(Cell::new(0));
    assert!(super::run::run_with(&target, &driver, &MockVault::working(), &committer).is_err());
    assert!(fs::read(&target)? == CANDIDATE);
    assert!(pending(&target)?.join(SNAPSHOT).exists());
    driver.fail_live_verify = false;
    assert!(run(&target, &driver, &MockVault::working()).is_err());
    Ok(())
}

#[test]
fn external_edit_is_not_overwritten_on_precommit_conflict() -> Result<()> {
    let (_dir, target) = setup()?;
    let mut driver = MockDriver::new(&target);
    driver.external_change = true;
    assert!(run(&target, &driver, &MockVault::working()).is_err());
    assert!(fs::read(&target)? == EXTERNAL);
    assert!(pending(&target)?.join(SNAPSHOT).exists());
    Ok(())
}

fn create_interrupted(target: &Path, phase: Phase) -> Result<()> {
    let recovery = pending(target)?;
    files::create_recovery(&recovery)?;
    MockVault::working().seal(&recovery.join(SNAPSHOT), ORIGINAL)?;
    let mut journal = Journal::new(ORIGINAL, ConfigFormat::Encrypted);
    journal.phase = phase;
    journal.candidate_digest = Some(files::digest(CANDIDATE));
    journal.write(&recovery)?;
    fs::write(target, CANDIDATE)?;
    Ok(())
}

#[test]
fn interrupted_commit_can_restore_authenticated_snapshot() -> Result<()> {
    let (_dir, target) = setup()?;
    create_interrupted(&target, Phase::Committing)?;
    assert!(matches!(recover_interrupted(&target, &MockVault::working())?, RecoveryOutcome::OriginalRestored { cleanup_pending: false }));
    assert!(fs::read(&target)? == ORIGINAL);
    Ok(())
}


fn create_plain_interrupted(target: &Path, phase: Phase) -> Result<()> {
    fs::write(target, PLAIN_ORIGINAL)?;
    let recovery = pending(target)?;
    files::create_recovery(&recovery)?;
    MockVault::working().seal(&recovery.join(SNAPSHOT), PLAIN_ORIGINAL)?;
    let mut journal = Journal::new(PLAIN_ORIGINAL, ConfigFormat::Plaintext);
    journal.phase = phase;
    journal.candidate_digest = Some(files::digest(PLAIN_CANDIDATE));
    journal.write(&recovery)?;
    fs::write(target, PLAIN_CANDIDATE)?;
    Ok(())
}

#[test]
fn interrupted_plaintext_commit_restores_authenticated_snapshot_without_temp() -> Result<()> {
    let (_dir, target) = setup()?;
    create_plain_interrupted(&target, Phase::Committing)?;
    assert!(matches!(recover_interrupted(&target, &MockVault::working())?, RecoveryOutcome::OriginalRestored { cleanup_pending: false }));
    assert!(fs::read(&target)? == PLAIN_ORIGINAL);
    Ok(())
}

#[test]
fn completed_verified_commit_is_not_rolled_back_on_cleanup_retry() -> Result<()> {
    let (_dir, target) = setup()?;
    create_interrupted(&target, Phase::Verified)?;
    assert!(matches!(recover_interrupted(&target, &MockVault::working())?, RecoveryOutcome::VerifiedCommitKept { cleanup_pending: false }));
    assert!(fs::read(&target)? == CANDIDATE);
    Ok(())
}

#[test]
fn recovery_refuses_unknown_current_config() -> Result<()> {
    let (_dir, target) = setup()?;
    create_interrupted(&target, Phase::Committing)?;
    fs::write(&target, EXTERNAL)?;
    assert!(recover_interrupted(&target, &MockVault::working()).is_err());
    assert!(fs::read(&target)? == EXTERNAL);
    assert!(pending(&target)?.exists());
    Ok(())
}

#[test]
fn recovery_wrong_identity_does_not_change_candidate() -> Result<()> {
    let (_dir, target) = setup()?;
    create_interrupted(&target, Phase::Committing)?;
    assert!(recover_interrupted(&target, &MockVault { fail_open: true, corrupt: false }).is_err());
    assert!(fs::read(&target)? == CANDIDATE);
    assert!(pending(&target)?.exists());
    Ok(())
}

#[test]
fn os_lock_blocks_second_owner_and_releases_on_drop() -> Result<()> {
    let (_dir, target) = setup()?;
    let first = files::ConfigLock::acquire(&target)?;
    assert!(files::ConfigLock::acquire(&target).is_err());
    drop(first);
    let _next = files::ConfigLock::acquire(&target)?;
    Ok(())
}

#[test]
fn encrypted_header_checks_first_meaningful_line_not_substring() {
    assert!(files::encrypted_header(ORIGINAL));
    assert!(!files::encrypted_header(b"[remote]\npassword=RCLONE_ENCRYPT_V0:\n"));
    assert!(!files::encrypted_header(b"RCLONE_ENCRYPT_V1:\n"));
}

#[test]
fn config_format_distinguishes_plaintext_and_encrypted() -> Result<()> {
    assert_eq!(files::format(ORIGINAL)?, ConfigFormat::Encrypted);
    assert_eq!(files::format(PLAIN_ORIGINAL)?, ConfigFormat::Plaintext);
    assert!(files::format(b"RCLONE_ENCRYPT_V1:\nunsupported\n").is_err());
    Ok(())
}
