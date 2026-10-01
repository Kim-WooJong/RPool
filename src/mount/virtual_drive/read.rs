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
    pub(crate) fn view(&self) -> Result<BTreeMap<String, Revision>> {
        let s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut result = BTreeMap::new();
        for (path, resolved) in s.resolved()? {
            if let Some(content) = resolved.event.content {
                result.insert(
                    path,
                    Revision::Cloud {
                        id: resolved.event_id,
                        content,
                    },
                );
            }
        }
        // A DAV transport cannot observe native rclone handle closure. Keep
        // served paths pinned for the mount session and expose incoming revisions
        // as named copies instead of mixing bytes across unconditioned ranges.
        for (name, pinned) in self
            .pins
            .lock()
            .unwrap()
            .iter()
            .filter(|_| self.pool_sync_roots.is_empty())
        {
            if let Some(current) = result.get(name).cloned() {
                if current.id() != pinned.id() {
                    let alternate = crate::mount::shared_model::conflict_path(
                        name,
                        "incoming",
                        current.id(),
                        0,
                    )?;
                    result.insert(alternate, current);
                    result.insert(name.clone(), pinned.clone());
                }
            } else {
                result.insert(name.clone(), pinned.clone());
            }
        }
        for intent in &s.pending {
            if intent.spool.is_some() {
                result.insert(
                    intent.path.clone(),
                    Revision::Local {
                        id: intent.id.clone(),
                        path: self.spool_path(intent),
                        size: intent.size,
                        _lease: self.local_lease(&intent.id),
                    },
                );
            } else {
                result.remove(&intent.path);
            }
        }
        Ok(result)
    }
    pub(crate) fn spool_path(&self, intent: &Intent) -> PathBuf {
        self.root.join("spool").join(&intent.id).join("content")
    }
    pub(crate) fn read(&self, revision: &Revision, offset: u64, count: usize) -> Result<Vec<u8>> {
        match revision {
            Revision::Cloud { content, .. } => self.cache.read(
                &crate::storage::reader::StorageReader::rclone(&self.rclone),
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
                        s.resolved()?.get(path).is_some_and(|r| &r.event_id == id)
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
