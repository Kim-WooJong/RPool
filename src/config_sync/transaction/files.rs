//! Building blocks of the rclone.conf restore transaction: the recovery
//! directory `<config>.rpool-restore/` (encrypted snapshot, encrypted staging
//! file, journal), the `<config>.rpool-lock` lock, config format detection and
//! the atomic commit. Used by `transaction::run` and `transaction::recovery`.
use super::super::age_vault::sync_directory;
use super::super::secret_process::MAX_SECRET_BYTES;
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, Permissions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// age-encrypted copy of the original config inside the recovery directory.
pub(super) const SNAPSHOT: &str = "rclone-config.snapshot.age";
/// Encrypted staging config inside the recovery directory (encrypted configs only).
pub(super) const STAGE: &str = "candidate.conf";
/// Journal file name inside the recovery directory.
const JOURNAL: &str = "transaction.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Representation of an rclone config file.
pub(super) enum ConfigFormat {
    /// rclone-encrypted (`RCLONE_ENCRYPT_V0:` header).
    Encrypted,
    /// Plain UTF-8 INI text.
    Plaintext,
}

/// Metadata of a regular, non-symlink file with a single hard link (Unix).
pub(super) fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let meta =
        fs::symlink_metadata(path).map_err(|_| anyhow!("configuration file is inaccessible"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("configuration must be a regular non-symlink file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            bail!("hard-linked configuration files are not supported");
        }
    }
    Ok(meta)
}

/// Canonical path of the config to update, which must be a regular file.
pub(super) fn target_path(input: &Path) -> Result<PathBuf> {
    regular_file(input)?;
    input
        .canonicalize()
        .map_err(|_| anyhow!("cannot resolve configuration path"))
}

/// `<target><suffix>` in the same directory.
pub(super) fn sibling(target: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("configuration parent is missing"))?;
    let mut name = target
        .file_name()
        .ok_or_else(|| anyhow!("configuration name is missing"))?
        .to_os_string();
    name.push(suffix);
    Ok(parent.join(name))
}

/// Recovery directory path: `<config>.rpool-restore`.
pub(super) fn recovery_path(target: &Path) -> Result<PathBuf> {
    sibling(target, ".rpool-restore")
}

/// Whether anything exists at `path` (symlinks included).
pub(super) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("cannot inspect transaction path"),
    }
}

/// Exclusive OS lock on `<config>.rpool-lock`, held for the whole transaction
/// or recovery; released when dropped. The lock file itself is kept.
pub(super) struct ConfigLock {
    /// Open lock file; the OS lock is released when it is closed.
    _file: File,
}
impl ConfigLock {
    /// Takes the lock without waiting; errors if another restore holds it or the
    /// filesystem has no locking.
    pub(super) fn acquire(target: &Path) -> Result<Self> {
        let path = sibling(target, ".rpool-lock")?;
        if exists(&path)? {
            regular_file(&path)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|_| anyhow!("cannot open configuration transaction lock"))?;
        match file.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => {
                bail!("another rpool restore holds this configuration lock")
            }
            Err(TryLockError::Error(_)) => {
                bail!("configuration locking is unavailable; restore refused")
            }
        }
        // Keep the sidecar inode permanently. Deleting it on unlock would permit
        // two lock owners on different inodes. Closing releases the OS lock.
        Ok(Self { _file: file })
    }
}

/// Whether the first non-comment line is rclone's `RCLONE_ENCRYPT_V0:` header.
/// Also used by the plaintext editor to refuse encrypted configs.
pub(in crate::config_sync) fn encrypted_header(bytes: &[u8]) -> bool {
    bytes
        .split(|b| *b == b'\n')
        .find_map(|line| {
            let line = match std::str::from_utf8(line) {
                Ok(s) => s.trim(),
                Err(_) => return Some(false),
            };
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                None
            } else {
                Some(line == "RCLONE_ENCRYPT_V0:")
            }
        })
        .unwrap_or(false)
}

/// Whether the first non-comment line is some other `RCLONE_ENCRYPT_*` header.
fn unsupported_encryption_header(bytes: &[u8]) -> bool {
    bytes
        .split(|b| *b == b'\n')
        .find_map(|line| {
            let line = match std::str::from_utf8(line) {
                Ok(s) => s.trim(),
                Err(_) => return Some(false),
            };
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                None
            } else {
                Some(line.starts_with("RCLONE_ENCRYPT_") && line != "RCLONE_ENCRYPT_V0:")
            }
        })
        .unwrap_or(false)
}

/// Detects the config format; errors on unknown encryption headers or non-UTF-8 plaintext.
pub(super) fn format(bytes: &[u8]) -> Result<ConfigFormat> {
    if encrypted_header(bytes) {
        return Ok(ConfigFormat::Encrypted);
    }
    if unsupported_encryption_header(bytes) {
        bail!("unsupported rclone configuration encryption format");
    }
    std::str::from_utf8(bytes)
        .map_err(|_| anyhow!("unsupported plaintext rclone configuration encoding"))?;
    Ok(ConfigFormat::Plaintext)
}

/// Reads a regular config file (1 byte..`MAX_SECRET_BYTES`) of a supported format.
pub(super) fn read_config(path: &Path) -> Result<SensitiveBytes> {
    let meta = regular_file(path)?;
    if meta.len() == 0 || meta.len() > MAX_SECRET_BYTES as u64 {
        bail!("unsupported configuration file size");
    }
    let mut bytes = SensitiveBytes(Vec::with_capacity(meta.len() as usize + 1));
    File::open(path)
        .map_err(|_| anyhow!("cannot read configuration"))?
        .take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_end(&mut bytes.0)
        .map_err(|_| anyhow!("cannot read configuration"))?;
    if bytes.0.len() > MAX_SECRET_BYTES {
        bail!("unsupported configuration file size");
    }
    format(&bytes.0)?;
    Ok(bytes)
}

/// BLAKE3 hex digest, used to identify original and candidate configs in the journal.
pub(super) fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Creates the recovery directory exclusively (0700 on Unix); fails if a pending
/// recovery already exists.
pub(super) fn create_recovery(path: &Path) -> Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();

    builder.create(path).map_err(|_| {
        anyhow!("pending recovery exists or transaction directory cannot be created")
    })?;
    sync_directory(
        path.parent()
            .ok_or_else(|| anyhow!("transaction parent is missing"))?,
    )
}

/// Writes encrypted staging bytes to a new file (0600 on Unix); refuses plaintext.
pub(super) fn stage_ciphertext(path: &Path, bytes: &[u8]) -> Result<()> {
    if format(bytes)? != ConfigFormat::Encrypted {
        bail!("refusing unencrypted staging data");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| anyhow!("cannot create encrypted staging config"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| anyhow!("cannot write encrypted staging config"))
}

/// Replaces the live config; a trait so tests can inject failing commits.
pub(super) trait Committer {
    /// Replaces `target` with `bytes` of `format`, keeping `permissions`.
    fn replace(
        &self,
        target: &Path,
        bytes: &[u8],
        permissions: &Permissions,
        format: ConfigFormat,
    ) -> Result<()>;
}

/// Real [`Committer`]: encrypted configs are replaced atomically via a sibling
/// temp file; plaintext configs are rewritten in place under a file lock, so no
/// second plaintext copy is created.
pub(super) struct ConfigCommit;
impl Committer for ConfigCommit {
    fn replace(
        &self,
        target: &Path,
        bytes: &[u8],
        permissions: &Permissions,
        format: ConfigFormat,
    ) -> Result<()> {
        if self::format(bytes)? != format {
            bail!("configuration representation changed unexpectedly");
        }
        regular_file(target)?;
        match format {
            ConfigFormat::Encrypted => {
                let parent = target
                    .parent()
                    .ok_or_else(|| anyhow!("configuration parent is missing"))?;
                let mut temp = tempfile::Builder::new()
                    .prefix(".rpool-cipher-")
                    .tempfile_in(parent)
                    .map_err(|_| anyhow!("cannot create encrypted replacement"))?;
                temp.write_all(bytes)
                    .map_err(|_| anyhow!("cannot write encrypted replacement"))?;
                temp.as_file()
                    .set_permissions(permissions.clone())
                    .map_err(|_| anyhow!("cannot preserve configuration permissions"))?;
                temp.as_file()
                    .sync_all()
                    .map_err(|_| anyhow!("cannot synchronize encrypted replacement"))?;
                // Ciphertext may use a sibling temp because it does not expose config credentials.
                temp.into_temp_path()
                    .persist(target)
                    .map_err(|_| anyhow!("atomic configuration replacement failed"))?;
                Ok(())
            }
            ConfigFormat::Plaintext => {
                // Deliberately no tempfile/backup: the live rclone.conf is already
                // plaintext, but no second plaintext-at-rest copy may be created.
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(target)
                    .map_err(|_| anyhow!("cannot open plaintext configuration for commit"))?;
                match file.try_lock() {
                    Ok(()) => (),
                    Err(TryLockError::WouldBlock) => {
                        bail!("plaintext configuration is locked by another process")
                    }
                    Err(TryLockError::Error(_)) => {
                        bail!("plaintext configuration locking is unavailable")
                    }
                }
                file.set_len(0)
                    .map_err(|_| anyhow!("cannot truncate plaintext configuration"))?;
                file.write_all(bytes)
                    .map_err(|_| anyhow!("cannot write plaintext configuration"))?;
                // Same inode: existing permissions/ACLs are left untouched.
                file.sync_all()
                    .map_err(|_| anyhow!("cannot synchronize plaintext configuration"))?;
                Ok(())
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Progress recorded in the journal.
pub(super) enum Phase {
    /// Recovery directory and snapshot are being created; the live config is untouched.
    Preparing,
    /// Candidate config is built and its digest recorded.
    Ready,
    /// The live config is being replaced.
    Committing,
    /// The new config was committed and verified; recovery keeps it.
    Verified,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Transaction journal stored as `transaction.json` in the recovery directory.
pub(super) struct Journal {
    /// Journal format version (currently 2).
    pub(super) schema_version: u32,
    /// Last reached phase.
    pub(super) phase: Phase,
    /// Format of the original config.
    pub(super) format: ConfigFormat,
    /// BLAKE3 of the original config.
    pub(super) original_digest: String,
    /// BLAKE3 of the candidate config; set from `Ready` on.
    pub(super) candidate_digest: Option<String>,
}
impl Journal {
    /// New journal in phase `Preparing` for `original`.
    pub(super) fn new(original: &[u8], format: ConfigFormat) -> Self {
        Self {
            schema_version: 2,
            phase: Phase::Preparing,
            format,
            original_digest: digest(original),
            candidate_digest: None,
        }
    }
    /// Atomically writes the journal into `dir` and fsyncs the directory.
    pub(super) fn write(&self, dir: &Path) -> Result<()> {
        let mut file = tempfile::Builder::new()
            .prefix(".journal-")
            .tempfile_in(dir)
            .map_err(|_| anyhow!("cannot create transaction journal"))?;
        serde_json::to_writer(&mut file, self)
            .map_err(|_| anyhow!("cannot write transaction journal"))?;
        file.as_file()
            .sync_all()
            .map_err(|_| anyhow!("cannot synchronize transaction journal"))?;
        file.into_temp_path()
            .persist(dir.join(JOURNAL))
            .map_err(|_| anyhow!("cannot commit transaction journal"))?;
        sync_directory(dir)
    }
    /// Reads and validates the journal of a recovery directory (size, schema,
    /// digests, candidate digest present after `Preparing`).
    pub(super) fn read(dir: &Path) -> Result<Self> {
        let meta =
            fs::symlink_metadata(dir).map_err(|_| anyhow!("recovery directory unavailable"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("unsafe recovery directory");
        }
        let path = dir.join(JOURNAL);
        if regular_file(&path)?.len() > 16384 {
            bail!("invalid recovery journal size");
        }
        let file = File::open(path).map_err(|_| anyhow!("recovery journal unavailable"))?;
        let journal: Self = serde_json::from_reader(file.take(16385))
            .map_err(|_| anyhow!("invalid recovery journal"))?;
        let is_digest = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        if journal.schema_version != 2
            || !is_digest(&journal.original_digest)
            || journal
                .candidate_digest
                .as_ref()
                .map(|s| !is_digest(s))
                .unwrap_or(false)
            || (journal.phase != Phase::Preparing && journal.candidate_digest.is_none())
        {
            bail!("unsupported recovery journal");
        }
        Ok(journal)
    }
}

/// Removes the recovery directory if it is a real directory; returns `false`
/// when it could not be removed or synced (cleanup then stays pending).
pub(super) fn cleanup_recovery(path: &Path) -> bool {
    // Only the exclusively-created transaction directory is eligible. Never
    // sweep *.age or recovery data from update-cleanup.nu.
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if fs::remove_dir_all(path).is_err() {
                return false;
            }
            path.parent()
                .map(|p| sync_directory(p).is_ok())
                .unwrap_or(false)
        }
        _ => false,
    }
}
