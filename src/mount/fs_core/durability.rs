//! Local durability acknowledgement: fsync, flush and freeze seal a file's
//! unsealed generation. None of them waits for cloud upload.
//!
//! A seal runs in two phases so a large file never stalls the rest of the
//! drive: `prehash` fsyncs and hashes the image holding no lock, then
//! `seal_file` (under the namespace and slot locks) only records the intent.
//! The acknowledgement point is unchanged: the call returns after the image is
//! flushed and the intent is durable, and a write that lands during the
//! unlocked phase invalidates the hash, so the seal re-hashes under the lock.
use super::core::{lock, FsCore};
use super::error::{FsError, FsResult};
use super::handles::HandleId;
use super::identity::FileId;

impl FsCore {
    /// Seal the file's unsealed writes. Clean files succeed without a new
    /// revision, so retried calls are idempotent. Unlinked files are `Stale`.
    pub(crate) fn fsync(&self, handle: HandleId) -> FsResult<()> {
        let file = lock(&self.handles)?.get(handle)?.file;
        self.prehash(file)?;
        let _namespace = self.shared()?;
        self.seal_file(file)
    }
    /// Frontend close/flush callback. Read-only handles have nothing to flush.
    pub(crate) fn flush(&self, handle: HandleId) -> FsResult<()> {
        if !lock(&self.handles)?.get(handle)?.write {
            return Ok(());
        }
        self.fsync(handle)
    }
    /// End the current generation (for example an NFS idle timeout). The next
    /// write begins a new generation from the sealed revision.
    pub(crate) fn freeze(&self, handle: HandleId) -> FsResult<()> {
        self.fsync(handle)
    }

    /// The slow half of a seal, with no lock held during I/O: flush and hash
    /// the file's unsealed image and cache the hash in its generation. A no-op
    /// without an unsealed generation, for an unlinked file (its writes are
    /// discarded, not sealed) or when the image is already hashed.
    pub(super) fn prehash(&self, file: FileId) -> FsResult<()> {
        if lock(&self.ids)?.path(file).is_none() {
            return Ok(());
        }
        let slot = lock(&self.slots)?.get(&file).cloned();
        let Some(slot) = slot else { return Ok(()) };
        let (intent, version) = match lock(&slot)?.as_ref() {
            Some(g) if !g.hashed() => (g.intent.clone(), g.version),
            _ => return Ok(()),
        };
        let (size, hash) = self.drive.hash_spool(&intent)?;
        if let Some(g) = lock(&slot)?.as_mut() {
            // A write meanwhile bumped the version; `seal_file` then re-hashes.
            if g.intent.id == intent.id {
                g.set_hashed(version, size, hash);
            }
        }
        Ok(())
    }

    /// Caller holds the namespace lock (shared or exclusive). Call `prehash`
    /// first (without the lock) so this only records the intent.
    pub(super) fn seal_file(&self, file: FileId) -> FsResult<()> {
        let slot = lock(&self.slots)?.get(&file).cloned();
        let Some(slot) = slot else { return Ok(()) };
        let mut generation = lock(&slot)?;
        let Some(current) = generation.as_ref() else {
            return Ok(());
        };
        let path = lock(&self.ids)?.path(file).ok_or(FsError::Stale)?;
        current.seal(&self.drive)?;
        *generation = None;
        // Acknowledged. Rebasing only serves reads after an unlink, so a failed
        // view refresh must not report the seal as failed.
        if let Ok(sealed) = self.visible(&path) {
            if let Some(revision) = &sealed {
                lock(&self.observed)?.insert(file, revision.clone());
            }
            lock(&self.handles)?.rebase(file, sealed);
        }
        Ok(())
    }
}
