//! Encrypted secret vault I/O through the external `age` tool: encrypts crypt
//! secret bundles and config snapshots to a recipient, and decrypts them with an
//! identity file that must live outside the portable artifact. Used by
//! `config_sync::export`, `import`, `crypt_restore` and the import transaction.
use super::secret_process::{execute, Output, MAX_SECRET_BYTES};
use crate::models::secrets::SecretBundle;
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Encryption has no identity/private-key argument.
pub(crate) struct AgeEncrypt<'a> {
    /// `age` executable name or path.
    pub(crate) executable: &'a Path,
    /// age public recipient the output is encrypted to.
    pub(crate) recipient: &'a str,
}

/// Decryption settings: `age` executable, identity file and the artifact root
/// the identity must stay outside of.
pub(crate) struct AgeDecrypt<'a> {
    /// `age` executable name or path.
    pub(crate) executable: &'a Path,
    /// age identity (private key) file; checked by [`checked_identity_path`].
    pub(crate) identity: &'a Path,
    /// B7 must supply the actual portable package/project root.
    pub(crate) artifact_root: &'a Path,
}

impl AgeEncrypt<'_> {
    /// Streams `writer`'s plaintext into `age --encrypt` and atomically replaces
    /// `output` with the ciphertext (temp file in the same directory, header check
    /// for binary age format, fsync of file and directory). Refuses a symlink or
    /// non-file destination.
    fn write<W>(&self, output: &Path, writer: W) -> Result<()>
    where
        W: FnOnce(&mut std::process::ChildStdin) -> Result<()> + Send,
    {
        if self.recipient.is_empty() || self.recipient.trim() != self.recipient {
            bail!("age recipient is required");
        }
        if let Ok(meta) = fs::symlink_metadata(output) {
            if !meta.is_file() || meta.file_type().is_symlink() {
                bail!("unsafe vault destination");
            }
        }
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temp = tempfile::Builder::new()
            .prefix(".rpool-age-")
            .tempfile_in(parent)
            .map_err(|_| anyhow!("cannot create ciphertext output"))?;
        let stdout = temp
            .as_file()
            .try_clone()
            .map_err(|_| anyhow!("cannot open ciphertext output"))?;
        let mut command = Command::new(self.executable);
        command
            .args(["--encrypt", "--recipient"])
            .arg(self.recipient);
        execute(&mut command, writer, Output::Ciphertext(stdout))?;
        temp.as_file()
            .sync_all()
            .map_err(|_| anyhow!("cannot synchronize ciphertext"))?;
        if temp
            .as_file()
            .metadata()
            .map_err(|_| anyhow!("cannot inspect ciphertext"))?
            .len()
            == 0
        {
            bail!("age did not produce ciphertext");
        }
        {
            let mut header = [0u8; 22];
            File::open(temp.path())
                .and_then(|mut file| file.read_exact(&mut header))
                .map_err(|_| anyhow!("invalid age ciphertext header"))?;
            if &header != b"age-encryption.org/v1\n" {
                bail!("age output is not a binary encrypted vault");
            }
        }
        // Close handles before replacement (important on Windows).
        temp.into_temp_path()
            .persist(output)
            .map_err(|_| anyhow!("cannot commit encrypted vault"))?;
        sync_directory(parent)?;
        Ok(())
    }

    /// Validates and encrypts a crypt [`SecretBundle`] (JSON) to `output`. Used by
    /// export and crypt secret generation.
    pub(crate) fn write_bundle(&self, output: &Path, bundle: &SecretBundle) -> Result<()> {
        bundle.validate()?;
        self.write(output, |stdin| {
            serde_json::to_writer(stdin, bundle)
                .map_err(|_| anyhow!("cannot stream secret bundle to age"))
        })
    }

    /// Encrypts a raw rclone config snapshot for the import transaction's recovery copy.
    pub(super) fn write_snapshot(&self, output: &Path, config_bytes: &[u8]) -> Result<()> {
        self.write(output, |stdin| {
            stdin
                .write_all(config_bytes)
                .map_err(|_| anyhow!("cannot stream config snapshot to age"))
        })
    }
}

/// Canonical identity path; errors unless it is a file outside `artifact_root`
/// (so a private key is never shipped inside the portable package).
pub(super) fn checked_identity_path(identity: &Path, artifact_root: &Path) -> Result<PathBuf> {
    let identity = identity
        .canonicalize()
        .map_err(|_| anyhow!("age identity is missing or inaccessible"))?;
    let root = artifact_root
        .canonicalize()
        .map_err(|_| anyhow!("portable artifact root is inaccessible"))?;
    if !identity.is_file() || identity.starts_with(&root) {
        bail!("age identity must be a file outside the portable artifact root");
    }
    Ok(identity)
}

impl AgeDecrypt<'_> {
    /// Decrypts `input` (a regular file, at most twice the secret size limit) with
    /// `age --decrypt`; bytes are released only after age exits successfully.
    pub(super) fn read_bytes(&self, input: &Path) -> Result<SensitiveBytes> {
        let identity = checked_identity_path(self.identity, self.artifact_root)?;
        let meta =
            fs::symlink_metadata(input).map_err(|_| anyhow!("encrypted vault is missing"))?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > (MAX_SECRET_BYTES as u64 * 2)
        {
            bail!("unsupported encrypted vault input");
        }
        let mut command = Command::new(self.executable);
        command
            .args(["--decrypt", "--identity"])
            .arg(identity)
            .arg(input);
        // execute checks the final age exit status before releasing any bytes.
        // An authenticated prefix of a truncated vault is never accepted.
        execute(&mut command, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))
    }

    /// Decrypts and validates a crypt [`SecretBundle`]. Used by `crypt_restore`.
    pub(crate) fn read_bundle(&self, input: &Path) -> Result<SecretBundle> {
        let raw = self.read_bytes(input)?;
        let bundle: SecretBundle =
            serde_json::from_slice(&raw.0).map_err(|_| anyhow!("invalid crypt secret payload"))?;
        bundle.validate()?;
        Ok(bundle)
    }
}

/// fsyncs a directory on Unix so a rename inside it is durable; a no-op elsewhere.
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| anyhow!("cannot synchronize directory"))?;
    #[cfg(not(unix))]
    let _ = path; // File data is synced; no claim of directory/power-loss durability on Windows.
    Ok(())
}
