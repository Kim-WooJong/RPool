//! Local durability acknowledgement: fsync, flush and freeze seal a file's
//! unsealed generation. None of them waits for cloud upload.
use super::core::{lock, FsCore};
use super::error::{FsError, FsResult};
use super::handles::HandleId;
use super::identity::FileId;

impl FsCore {
    /// Seal the file's unsealed writes. Clean files succeed without a new
    /// revision, so retried calls are idempotent. Unlinked files are `Stale`.
    pub(crate) fn fsync(&self, handle: HandleId) -> FsResult<()> {
        let _namespace = self.shared()?;
        let file = lock(&self.handles)?.get(handle)?.file;
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

    /// Caller holds the namespace lock (shared or exclusive).
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
