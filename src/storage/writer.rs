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
struct ReadbackFailed;
impl std::fmt::Display for ReadbackFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upload acknowledged but readback failed")
    }
}

pub(crate) fn upload_retry(error: &anyhow::Error, attempt: u32) -> Option<std::time::Duration> {
    // A readback failure must NEVER replay the acknowledged write transaction.
    if error.downcast_ref::<ReadbackFailed>().is_some() {
        return None;
    }
    super::scheduler::read_retry(error, attempt)
}

pub(crate) struct StorageWriter {
    reader: StorageReader,
    native: Option<NativeCrypt>,
}
impl StorageWriter {
    pub(crate) fn rclone(executable: &str) -> Self {
        Self {
            reader: StorageReader::rclone(executable),
            native: None,
        }
    }
    /// Writer for a pool: native crypt writes when the pool opts in.
    pub(crate) fn for_pool(executable: &str, native_crypt: bool) -> Self {
        if !native_crypt {
            return Self::rclone(executable);
        }
        Self::native(RcloneContext::inherited(executable))
    }
    pub(crate) fn native(context: RcloneContext) -> Self {
        Self {
            reader: StorageReader::with_rclone_context(context.clone()),
            native: Some(NativeCrypt::new(context)),
        }
    }
    #[cfg(test)]
    pub(crate) fn synthetic(reader: StorageReader) -> Self {
        Self {
            reader,
            native: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn is_native(&self) -> bool {
        self.native.is_some()
    }
    pub(crate) fn reader(&self) -> &StorageReader {
        &self.reader
    }
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
        if self.reusable(shard)? {
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
            match backend.write(
                self.reader.operation_context(),
                &key,
                &mut input,
                &WriteOptions::default(),
            ) {
                Ok(receipt) => {
                    if receipt.size != shard.size || input.stream_position()? != shard.size {
                        return Err(StorageError::unknown_outcome(
                            "write receipt/source consumption mismatch",
                        )
                        .into());
                    }
                    // Readback failure never restarts the mutation in this call.
                    self.reader
                        .verify(shard, true)
                        .map_err(|e| e.context(ReadbackFailed))?;
                    super::rclone::traffic::credit_verified(&shard.object, shard.size);
                    return Ok(());
                }
                Err(error) if error.is_retriable() && attempt < retries.max(1) => {}
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!()
    }
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
    pub(crate) fn delete(&self, raw: &str) -> Result<()> {
        let (backend, key) = self.reader.resolve(raw)?;
        self.reader.forget_verified(raw);
        backend.delete(self.reader.operation_context(), &key)?;
        Ok(())
    }
}
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
