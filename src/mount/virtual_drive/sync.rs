//! Pulling remote history and synchronizing pending intents.

use super::*;

impl VirtualDrive {
    /// Metadata refresh never uploads dirty spool.
    pub(crate) fn pull(&self) -> Result<()> {
        if self.peer_retention {
            return self.pull_snapshots();
        }
        if !self.pool_sync_roots.is_empty() {
            return self.pull_pool();
        }
        if self.bounded_shared {
            return self.pull_checkpoint();
        }
        if let Some(root) = &self.shared_root {
            let transport = SharedTransport::new(&self.rclone, root)?;
            let known = self.state.lock().unwrap().events.clone();
            let bytes = transport.list_missing(&known.keys().cloned().collect())?;
            let mut events = BTreeMap::new();
            for (id, bytes) in bytes {
                events.insert(id, serde_json::from_slice::<Event>(&bytes)?);
            }
            let mut s = self.state.lock().unwrap();
            let mut next = s.clone();
            let incoming_ids: Vec<_> = events.keys().cloned().collect();
            next.ingest(events)?;
            next.published.extend(incoming_ids);
            next.save(&self.root)?;
            *s = next;
        }
        Ok(())
    }
    /// Uploads pending intents. The capacity snapshot is not cleared here:
    /// it already reserves pending sizes, and `quota` reports no additional
    /// space as soon as pending intents or namespace events differ from it.
    pub(crate) fn sync(&self) -> Result<()> {
        // Never publish into a drive generation a pool migration froze or replaced.
        crate::mount::adoption_fence::check_publish(self)?;
        if self.peer_retention {
            return self.sync_snapshots();
        }
        if self.bounded_shared {
            return self.sync_checkpoint();
        }
        if self.root.join("retention-journal.json").exists() {
            bail!("resume interrupted retention before syncing");
        }
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync lock poisoned"))?;
        self.pull()?;
        let pending = self.state.lock().unwrap().pending.clone();
        for intent in pending {
            let content = if intent.spool.is_some() {
                let source = self.spool_path(&intent);
                if fs::metadata(&source)?.len() != intent.size
                    || crate::utils::hash_file_range(&source, 0, intent.size)? != intent.hash
                {
                    bail!("pending spool integrity failure");
                }
                let upload_dir = source.parent().unwrap().join("upload");
                fs::create_dir_all(&upload_dir)?;
                let name = Path::new(&intent.path)
                    .file_name()
                    .context("intent filename missing")?;
                let staged = upload_dir.join(name);
                if !staged.exists() {
                    fs::hard_link(&source, &staged)?;
                }
                let archive_id = format!("virtual-{}", intent.id);
                // Once the fallback uploader starts, a later eligibility change
                // must not switch this identity to a different composite manifest.
                let full_route = source.parent().unwrap().join("full-upload.json");
                let already_full = if full_route.exists() {
                    let recorded: String = crate::utils::read_json(&full_route)?;
                    if recorded != archive_id {
                        bail!("upload route identity mismatch; preserve pending data");
                    }
                    true
                } else {
                    // Resume a full upload started by an older binary, before
                    // per-intent route receipts were introduced.
                    fs::read_dir(&upload_dir)?.try_fold(false, |found, entry| {
                        let entry = entry?;
                        Ok::<_, std::io::Error>(
                            found
                                || entry.file_type()?.is_dir()
                                    && entry.file_name().to_string_lossy().starts_with("eligible-"),
                        )
                    })?
                };
                let base = if self.pool_sync_roots.is_empty() || already_full {
                    None
                } else {
                    self.state.lock().unwrap().upload_base(&intent)?
                };
                let incremental = match base {
                    Some(base) => crate::mount::incremental::upload(
                        &self.rclone,
                        &self.policy,
                        &self.pool,
                        &staged,
                        &archive_id,
                        &base.manifest,
                    )?,
                    None => None,
                };
                let manifest = match incremental {
                    Some(manifest) => manifest,
                    None => {
                        crate::mount::namespace::durable_json(&full_route, &archive_id)?;
                        let (manifest, manifest_remotes) =
                            crate::mount::workspace::upload_eligible_tracked(
                                &self.rclone,
                                &self.policy,
                                &self.pool,
                                &staged,
                                &archive_id,
                            )?;
                        self.record_owned_archive(&intent, &manifest, &manifest_remotes)?;
                        manifest
                    }
                };
                // Composite peer manifests borrow immutable objects from older
                // archives. They must never enter the exclusive ownership ledger.
                Some(Content {
                    hash: intent.hash.clone(),
                    size: intent.size,
                    manifest,
                })
            } else {
                None
            };
            self.commit_uploaded(&intent, content)?;
        }
        if !self.pool_sync_roots.is_empty() {
            self.publish_pool()?;
        } else if let Some(root) = &self.shared_root {
            let transport = SharedTransport::new(&self.rclone, root)?;
            let unpublished: Vec<_> = {
                let s = self.state.lock().unwrap();
                s.unpublished_ordered()?
            };
            for (id, event) in unpublished {
                transport.publish(&id, &serde_json::to_vec(&event)?)?;
                let mut s = self.state.lock().unwrap();
                let mut next = s.clone();
                next.published.insert(id);
                next.save(&self.root)?;
                *s = next;
            }
        }
        self.cleanup_committed_spool()?;
        Ok(())
    }
    pub(crate) fn commit_uploaded(&self, intent: &Intent, content: Option<Content>) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let mut next = s.clone();
        let id = next.commit(intent, content.clone())?;
        next.save(&self.root)?;
        *s = next;
        if let Some(content) = content {
            let mut pins = self.pins.lock().unwrap();
            if pins.get(&intent.path).is_some_and(|r| r.id() == intent.id) {
                pins.insert(intent.path.clone(), Revision::Cloud { id, content });
            }
        }
        Ok(())
    }
}
