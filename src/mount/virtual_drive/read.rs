//! Visible namespace view, local revision leases and revision reads/pins.

use super::*;

impl VirtualDrive {
    pub(crate) fn local_lease(&self, id: &str) -> Arc<()> {
        let mut leases = self.local_leases.lock().unwrap();
        if let Some(lease) = leases.get(id).and_then(std::sync::Weak::upgrade) {
            return lease;
        }
        let lease = Arc::new(());
        leases.insert(id.into(), Arc::downgrade(&lease));
        lease
    }
    /// Every visible file with its revision. Prefer [`Self::visible`] or
    /// [`Self::visible_revision`]: this clones the whole namespace.
    pub(crate) fn view(&self) -> Result<BTreeMap<String, Revision>> {
        let (s, projection, _) = self.locked_visible()?;
        self.full_view(&s, &projection)
    }
    pub(crate) fn spool_path(&self, intent: &Intent) -> PathBuf {
        self.root.join("spool").join(&intent.id).join("content")
    }
    pub(crate) fn read(&self, revision: &Revision, offset: u64, count: usize) -> Result<Vec<u8>> {
        match revision {
            // Background shard downloads may outlive this call, so they share
            // the reader instead of borrowing it.
            Revision::Cloud { content, .. } => self.cache.read_shared(
                &Arc::new(crate::storage::reader::StorageReader::rclone(&self.rclone)),
                &content.manifest,
                offset,
                count,
                self.policy.workers,
                self.policy.retries,
            ),
            Revision::Local { path, size, .. } => {
                if offset >= *size {
                    return Ok(vec![]);
                }
                let mut f = File::open(path)?;
                f.seek(SeekFrom::Start(offset))?;
                let mut bytes = vec![0; (count as u64).min(size - offset) as usize];
                f.read_exact(&mut bytes)?;
                Ok(bytes)
            }
        }
    }
    pub(crate) fn pin_read(&self, path: &str, revision: &Revision) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if !self.pool_sync_roots.is_empty() {
            // Native DAV clients may make independent unconditioned range requests.
            // Once bytes were served, reject a changed revision until remount rather
            // than assemble ranges from different versions of the same pathname.
            let current = if let Some(intent) = s.pending.iter().rev().find(|i| i.path == path) {
                intent.spool.is_some() && intent.id == revision.id()
            } else {
                match revision {
                    Revision::Cloud { id, .. } => {
                        let (projection, _) = self.cached_visible(&mut s)?;
                        projection.committed(path).is_some_and(|r| r.id() == id)
                    }
                    Revision::Local { .. } => false,
                }
            };
            if !current {
                bail!("pool file changed during open; refresh and retry");
            }
            let mut reads = self.peer_read_pins.lock().unwrap();
            if reads.get(path).is_some_and(|id| id != revision.id()) {
                bail!("pool file changed after a prior read; remount to avoid mixing revisions");
            }
            reads.insert(path.into(), revision.id().into());
            if let Revision::Cloud { id, .. } = revision {
                s.bases.insert(path.into(), vec![id.clone()]);
            }
        }
        match revision {
            Revision::Cloud { id, .. } => {
                s.bases
                    .entry(path.into())
                    .or_insert_with(|| vec![id.clone()]);
            }
            Revision::Local { .. } => {}
        }
        s.save(&self.root)?;
        self.pins
            .lock()
            .unwrap()
            .entry(path.into())
            .or_insert_with(|| revision.clone());
        Ok(())
    }
}
