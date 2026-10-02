//! Verified archive mutations. Writes go through rclone crypt, or, for pools with
//! `native_crypt`, through RPool's own rclone-compatible encryption onto the crypt
//! remote's base. Readback always uses rclone crypt, so every native write is
//! proven readable by rclone. Synthetic routes are available only in tests.
use super::error::StorageError;
use super::native_crypt::route::NativeCrypt;
use super::rclone::RcloneContext;
use super::reader::{is_recoverable_loss, StorageReader};
use super::traits::WriteOptions;
use crate::prelude::*;

#[derive(Debug)]
/// Error context marking that a write was acknowledged but its readback failed;
/// [`upload_retry`] never retries it.
struct ReadbackFailed;
impl std::fmt::Display for ReadbackFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upload acknowledged but readback failed")
    }
}

/// Retry policy for shard uploads (`commands::put`): like `scheduler::read_retry`,
/// except an acknowledged write whose readback failed is never replayed.
pub(crate) fn upload_retry(error: &anyhow::Error, attempt: u32) -> Option<std::time::Duration> {
    // A readback failure must NEVER replay the acknowledged write transaction.
    if error.downcast_ref::<ReadbackFailed>().is_some() {
        return None;
    }
    super::scheduler::read_retry(error, attempt)
}

/// `RPOOL_UPLOAD_VERIFY=readback` always reads uploads back in full, even
/// when the provider's hash already proved them.
fn full_readback_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        std::env::var("RPOOL_UPLOAD_VERIFY").is_ok_and(|v| v.eq_ignore_ascii_case("readback"))
    })
}

/// Verified archive mutations: shard/metadata uploads with readback, migration
/// copies and deletes. Built per pool by `put`, migration, replication and the
/// virtual drive.
pub(crate) struct StorageWriter {
    /// Verified reader used for readback, reuse checks and address resolution.
    reader: StorageReader,
    /// Native crypt writer (`None` = writes go through rclone crypt).
    native: Option<NativeCrypt>,
    /// Set by `put` for one archive upload (`upload_session`).
    pub(super) session: Mutex<Option<super::upload_session::UploadSession>>,
}
impl StorageWriter {
    /// Writer that writes through rclone crypt with the inherited rclone config.
    pub(crate) fn rclone(executable: &str) -> Self {
        Self {
            reader: StorageReader::rclone(executable),
            native: None,
            session: Mutex::new(None),
        }
    }
    /// Writer for a pool: native crypt writes when the pool opts in.
    pub(crate) fn for_pool(executable: &str, native_crypt: bool) -> Self {
        if !native_crypt {
            return Self::rclone(executable);
        }
        Self::native(RcloneContext::inherited(executable))
    }
    /// Writer with RPool's native crypt on `context`; readback still goes through rclone crypt.
    pub(crate) fn native(context: RcloneContext) -> Self {
        Self {
            reader: StorageReader::with_rclone_context(context.clone()),
            native: Some(NativeCrypt::new(context)),
            session: Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(crate) fn synthetic(reader: StorageReader) -> Self {
        Self {
            reader,
            native: None,
            session: Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(crate) fn is_native(&self) -> bool {
        self.native.is_some()
    }
    /// The verified reader behind this writer.
    pub(crate) fn reader(&self) -> &StorageReader {
        &self.reader
    }
    /// Checks that `raw` is a writable crypt destination (native route or rclone crypt
    /// gate) before anything is written. Called by journals, migration and replication.
    pub(crate) fn ensure_destination(&self, raw: &str) -> Result<()> {
        if let Some(native) = &self.native {
            native.ensure(self.reader.operation_context(), raw)?;
        } else if let Some(context) = self.reader.legacy_context() {
            context.ensure_crypt(self.reader.operation_context(), raw)?;
        } else {
            #[cfg(not(test))]
            return Err(StorageError::unsupported("native archive encryption binding").into());
        }
        Ok(())
    }
    /// Reuse requires the expected content, never just its length. Operational
    /// errors propagate; they are not permission to overwrite an unknown object.
    pub(crate) fn reusable(&self, shard: &Shard) -> Result<bool> {
        self.ensure_destination(&shard.object)?;
        match self.reader.verify_unchanged(shard) {
            Ok(()) => Ok(true),
            Err(error) if is_recoverable_loss(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }
    /// Uploads `shard.size` bytes of `path` from `offset` as `shard.object`: stages
    /// them in a private spool, checks their BLAKE3, skips already-verified objects
    /// (unless the session is fresh), writes with retries, then proves the stored object
    /// by provider hash, deferred session check or full readback.
    /// Called by `storage::data_upload`.
    pub(crate) fn write_file(
        &self,
        path: &Path,
        offset: u64,
        shard: &Shard,
        retries: u32,
    ) -> Result<()> {
        self.ensure_destination(&shard.object)?;
        // Own a stable, bounded disk spool so retries cannot read changing input.
        let spool = tempfile::tempdir()?;
        let staged = spool.path().join("logical-bytes");
        let mut input = File::open(path)?;
        let end = offset
            .checked_add(shard.size)
            .ok_or_else(|| anyhow!("source range overflow"))?;
        if input.metadata()?.len() < end {
            bail!("source size changed");
        }
        input.seek(SeekFrom::Start(offset))?;
        let mut output = File::create(&staged)?;
        let count = std::io::copy(&mut input.take(shard.size), &mut output)?;
        output.flush()?;
        drop(output);
        if count != shard.size
            || crate::utils::hash_file_range(&staged, 0, shard.size)? != shard.blake3
        {
            bail!("source content changed before upload");
        }
        let (fresh, defer) = self.session_mode();
        // A first attempt at a fresh archive cannot find its own objects, so
        // the probe is skipped; resumed uploads still reuse verified ones.
        if !fresh && self.reusable(shard)? {
            return Ok(());
        }
        // The object is about to be replaced: its new readback is always full.
        self.reader.forget_verified(&shard.object);
        let routed = match &self.native {
            Some(native) => native.route(self.reader.operation_context(), &shard.object)?,
            None => None,
        };
        let (backend, key) = match routed {
            Some(route) => route,
            None => self.reader.resolve(&shard.object)?,
        };
        for attempt in 1..=retries.max(1) {
            let mut input = File::open(&staged)?;
            let options = WriteOptions {
                defer_hash_check: defer && self.native.is_some() && !full_readback_forced(),
                ..WriteOptions::default()
            };
            match backend.write(self.reader.operation_context(), &key, &mut input, &options) {
                Ok(receipt) => {
                    if receipt.size != shard.size || input.stream_position()? != shard.size {
                        return Err(StorageError::unknown_outcome(
                            "write receipt/source consumption mismatch",
                        )
                        .into());
                    }
                    if let Some(expected) = receipt.stored_hash {
                        // Checked with the rest of the archive (one listing
                        // per account) when the session finishes.
                        self.defer_check(shard.clone(), expected);
                        return Ok(());
                    }
                    // Readback failure never restarts the mutation in this call.
                    if receipt.hash_verified && !full_readback_forced() {
                        // The provider reported the hash of exactly the
                        // ciphertext sent: no need to download it again.
                        self.reader
                            .record_hash_verified(shard)
                            .map_err(|e| e.context(ReadbackFailed))?;
                    } else {
                        self.reader
                            .verify(shard, true)
                            .map_err(|e| e.context(ReadbackFailed))?;
                    }
                    super::rclone::traffic::credit_verified(&shard.object, shard.size);
                    return Ok(());
                }
                Err(error) if error.is_retriable() && attempt < retries.max(1) => {}
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!()
    }
    /// Uploads an in-memory metadata object (manifest replica) via [`Self::write_file`].
    pub(crate) fn write_bytes(&self, destination: &str, bytes: &[u8], retries: u32) -> Result<()> {
        let spool = tempfile::tempdir()?;
        let path = spool.path().join("metadata");
        fs::write(&path, bytes)?;
        self.write_file(
            &path,
            0,
            &descriptor(
                destination,
                bytes.len() as u64,
                blake3::hash(bytes).to_hex().to_string(),
            ),
            retries,
        )
    }
    /// Portable logical-byte transfer, NOT server-side/native ciphertext copy.
    pub(crate) fn copy_verified(
        &self,
        source: &Shard,
        destination: &str,
        retries: u32,
    ) -> Result<()> {
        self.ensure_destination(destination)?;
        let spool = tempfile::tempdir()?;
        let path = spool.path().join("migration");
        self.reader.download(source, &path, retries, false)?;
        let mut target = source.clone();
        target.object = destination.to_owned();
        self.write_file(&path, 0, &target, retries)
    }
    /// Deletes the object `raw` and forgets any in-process proof for it.
    pub(crate) fn delete(&self, raw: &str) -> Result<()> {
        let (backend, key) = self.reader.resolve(raw)?;
        self.reader.forget_verified(raw);
        backend.delete(self.reader.operation_context(), &key)?;
        Ok(())
    }
}
/// A data-shard descriptor for a single object (index/offset 0, no remote), used for
/// metadata uploads and tests.
pub(super) fn descriptor(object: &str, size: u64, blake3: String) -> Shard {
    Shard {
        index: 0,
        offset: 0,
        size,
        remote: String::new(),
        object: object.to_owned(),
        blake3,
        kind: ShardKind::Data,
        group: 0,
        slot: 0,
    }
}
