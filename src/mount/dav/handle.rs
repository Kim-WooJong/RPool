//! Open DAV file handles: pinned revision reads and spool-backed writes.

use super::*;

/// One open DAV file. Read handles serve a pinned `revision`; write handles own an intent and
/// its spool file, sealed on `flush`.
pub(super) struct Handle {
    /// Keeps the intent's local lease alive while the handle is open (write handles only).
    pub(super) _write_lease: Option<Arc<()>>,
    /// Drive that serves reads and seals writes.
    pub(super) drive: Arc<VirtualDrive>,
    /// Revision read from (and, for writes, the visible one the write descends from).
    pub(super) revision: Option<Revision>,
    /// Write intent; `None` for read-only handles.
    pub(super) intent: Option<Intent>,
    /// Spool file being written; `None` for read-only handles.
    pub(super) file: Option<File>,
    /// Read position in `revision` (bytes); writes use the spool file's own position.
    pub(super) offset: u64,
    /// Request body length from the open options, checked against `received` before sealing.
    pub(super) expected: Option<u64>,
    /// Body bytes written through this handle.
    pub(super) received: u64,
    /// Intent already sealed; further writes are refused.
    pub(super) sealed: bool,
    /// Write counters shared with the server.
    pub(super) write_stats: Arc<WriteStats>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevisionHandle")
            .field("offset", &self.offset)
            .finish()
    }
}

impl DavFile for Handle {
    fn metadata(&mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let (size, tag) = if let Some(file) = &self.file {
                (
                    file.metadata().map_err(failure)?.len(),
                    self.intent.as_ref().unwrap().id.clone(),
                )
            } else {
                let r = self.revision.as_ref().ok_or(FsError::NotFound)?;
                (r.size(), r.id().into())
            };
            Ok(Box::new(Meta {
                size,
                directory: false,
                tag,
            }) as _)
        })
    }
    fn write_buf(&mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed {
                return Err(FsError::Forbidden);
            }
            let f = self.file.as_mut().ok_or(FsError::Forbidden)?;
            while buf.has_remaining() {
                let chunk = buf.chunk();
                let n = chunk.len();
                let received = self
                    .received
                    .checked_add(n as u64)
                    .ok_or(FsError::GeneralFailure)?;
                self.drive.write_spool_bytes(f, chunk).map_err(failure)?;
                self.received = received;
                self.write_stats
                    .body_bytes
                    .fetch_add(n as u64, Ordering::Relaxed);
                buf.advance(n);
            }
            Ok(())
        })
    }
    fn write_bytes(&mut self, bytes: Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed {
                return Err(FsError::Forbidden);
            }
            let received = self
                .received
                .checked_add(bytes.len() as u64)
                .ok_or(FsError::GeneralFailure)?;
            self.drive
                .write_spool_bytes(self.file.as_mut().ok_or(FsError::Forbidden)?, &bytes)
                .map_err(failure)?;
            self.received = received;
            self.write_stats
                .body_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        })
    }
    fn read_bytes(&mut self, count: usize) -> FsFuture<'_, Bytes> {
        Box::pin(async move {
            if let Some(file) = &mut self.file {
                let mut bytes = vec![0; count];
                let n = file.read(&mut bytes).map_err(failure)?;
                bytes.truncate(n);
                return Ok(Bytes::from(bytes));
            }
            let drive = self.drive.clone();
            let revision = self.revision.clone().ok_or(FsError::Forbidden)?;
            let offset = self.offset;
            let bytes = tokio::task::spawn_blocking(move || drive.read(&revision, offset, count))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            self.offset += bytes.len() as u64;
            Ok(Bytes::from(bytes))
        })
    }
    fn seek(&mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move {
            if let Some(file) = &mut self.file {
                return file.seek(pos).map_err(failure);
            }
            let size = self.revision.as_ref().ok_or(FsError::NotFound)?.size();
            let n = match pos {
                SeekFrom::Start(n) => n as i128,
                SeekFrom::Current(n) => self.offset as i128 + n as i128,
                SeekFrom::End(n) => size as i128 + n as i128,
            };
            if n < 0 || n > u64::MAX as i128 {
                return Err(FsError::Forbidden);
            }
            self.offset = n as u64;
            Ok(self.offset)
        })
    }
    fn flush(&mut self) -> FsFuture<'_, ()> {
        Box::pin(async move {
            if self.sealed || self.file.is_none() {
                return Ok(());
            }
            let file = self.file.as_ref().unwrap();
            file.sync_all().map_err(failure)?;
            // OpenOptions::size is the request body length, not the resulting
            // file length for PATCH and Content-Range writes. dav-server calls
            // flush before its own premature-EOF check, so verify before seal.
            if self.expected.is_some_and(|n| self.received != n) {
                self.write_stats.incomplete.fetch_add(1, Ordering::Relaxed);
                return Err(FsError::GeneralFailure);
            }
            let drive = self.drive.clone();
            let intent = self.intent.clone().unwrap();
            tokio::task::spawn_blocking(move || drive.seal(intent))
                .await
                .map_err(failure)?
                .map_err(failure)?;
            self.sealed = true;
            let seals = self.write_stats.seals.fetch_add(1, Ordering::Relaxed) + 1;
            // Keep a bounded trace if an actual NFS run cannot reach clean stop.
            if seals.is_multiple_of(64) {
                eprintln!(
                    "RPool DAV write progress: seals={} write_opens={} body_bytes={} baseline_copy_bytes={}",
                    seals,
                    self.write_stats.write_opens.load(Ordering::Relaxed),
                    self.write_stats.body_bytes.load(Ordering::Relaxed),
                    self.write_stats.baseline_copy_bytes.load(Ordering::Relaxed),
                );
            }
            Ok(())
        })
    }
}
