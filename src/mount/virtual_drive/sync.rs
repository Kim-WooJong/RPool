//! Pulling remote history and synchronizing pending intents.

use super::*;

impl VirtualDrive {
    /// Metadata refresh never uploads dirty spool. Without pool-sync roots
    /// (test fixture only) the namespace is local and there is nothing to pull.
    pub(crate) fn pull(&self) -> Result<()> {
        if self.pool_sync_roots.is_empty() {
            return Ok(());
        }
        self.pull_pool()
    }
    /// One-shot synchronization (`--sync-only`, import batches, history
    /// apply, pool transitions): pull, upload every pending intent (a few
    /// files at a time), publish. Failed intents do not stop the others; the
    /// call still fails when any intent stayed pending. The capacity snapshot
    /// is not cleared here: it already reserves pending sizes, and `quota`
    /// reports no additional space as soon as pending intents or namespace
    /// events differ from it.
    pub(crate) fn sync(&self) -> Result<()> {
        self.sync_with(&|intent| self.upload_intent(intent))
    }
    /// [`Self::sync`] over an injectable per-intent upload (tests).
    pub(crate) fn sync_with(&self, upload: &upload_round::UploadFn<'_>) -> Result<()> {
        // Never publish into a drive generation a pool migration froze or replaced.
        crate::mount::adoption_fence::check_publish(self)?;
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync lock poisoned"))?;
        self.pull()?;
        let report = self.upload_round(
            &mut upload_retry::RetryBook::default(),
            &std::sync::atomic::AtomicBool::new(false),
            upload,
        );
        self.publish_and_clean()?;
        if let Some(first) = report.first_error {
            bail!(
                "{} pending upload(s) failed, {} completed; local data retained. First: {first}",
                report.failed,
                report.committed
            );
        }
        Ok(())
    }
    /// The mount's upload pass (`mount::upload_worker`): no metadata pull
    /// (the background interval pulls separately; commits use each intent's
    /// captured ancestry, and publication orders parents first by itself),
    /// failures back off in `book`. `Err` only for the fence or publication.
    pub(crate) fn upload_pending(
        &self,
        book: &mut upload_retry::RetryBook,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<upload_round::RoundReport> {
        self.upload_pending_with(book, cancelled, &|intent| self.upload_intent(intent))
    }
    /// [`Self::upload_pending`] over an injectable per-intent upload (tests).
    pub(crate) fn upload_pending_with(
        &self,
        book: &mut upload_retry::RetryBook,
        cancelled: &std::sync::atomic::AtomicBool,
        upload: &upload_round::UploadFn<'_>,
    ) -> Result<upload_round::RoundReport> {
        crate::mount::adoption_fence::check_publish(self)?;
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync lock poisoned"))?;
        let report = self.upload_round(book, cancelled, upload);
        drop(_gate);
        if self
            .publisher_running
            .load(std::sync::atomic::Ordering::Acquire)
        {
            // The mount's publisher publishes in the background, so the next
            // upload pass starts at once instead of waiting for every
            // account to take the new metadata records.
            self.publish.notify();
            return Ok(report);
        }
        // A round can run long: re-check the (cached) fence before publishing.
        crate::mount::adoption_fence::check_publish(self)?;
        self.publish_and_clean()?;
        Ok(report)
    }
    /// Publishes committed metadata and removes committed spool images; one
    /// caller at a time (the uploader, the publisher thread, `sync`).
    pub(crate) fn publish_and_clean(&self) -> Result<()> {
        let _publishing = self
            .publish_gate
            .lock()
            .map_err(|_| anyhow!("publish lock poisoned"))?;
        if !self.pool_sync_roots.is_empty() {
            self.publish_pool()?;
        }
        self.cleanup_committed_spool()?;
        Ok(())
    }
    /// Uploads one pending intent's spool (resumable; incremental against
    /// its captured base when possible). Deletions upload nothing.
    pub(super) fn upload_intent(&self, intent: &Intent) -> Result<Option<Content>> {
        if intent.spool.is_none() {
            return Ok(None);
        }
        let content = {
            let source = self.spool_path(intent);
            // The sealed spool image is immutable and was hashed at seal;
            // rehashing all of it here only delayed the first shard. Every
            // shard is hashed again as it uploads.
            if fs::metadata(&source)?.len() != intent.size {
                bail!("pending spool integrity failure");
            }
            if let Some(existing) = self.same_content(intent)? {
                // A copy (or re-save) of a file the drive already stores:
                // reference its archive instead of uploading the bytes again.
                return Ok(Some(existing));
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
                self.state.lock().unwrap().upload_base(intent)?
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
                    let (manifest, _) = crate::mount::upload::upload_eligible_tracked(
                        &self.rclone,
                        &self.policy,
                        &self.pool,
                        &staged,
                        &archive_id,
                    )?;
                    manifest
                }
            };
            Content {
                hash: intent.hash.clone(),
                size: intent.size,
                manifest,
            }
        };
        Ok(Some(content))
    }
    /// Content of a current cloud file with exactly the bytes of `intent`
    /// (same BLAKE3 of the whole file, same size), if any. Only current
    /// files count: their archives are kept by cleanup, while those of old,
    /// unretained revisions may already be deleted. Sharing is safe because
    /// archives are immutable: editing either file later writes a new one.
    pub(super) fn same_content(&self, intent: &Intent) -> Result<Option<Content>> {
        if intent.size == 0 || intent.hash.is_empty() {
            return Ok(None);
        }
        Ok(self
            .view()?
            .into_values()
            .find_map(|revision| match revision {
                Revision::Cloud { content, .. }
                    if content.size == intent.size && content.hash == intent.hash =>
                {
                    Some(content)
                }
                _ => None,
            }))
    }
    /// Commits an uploaded (or deletion) intent, saves the namespace, and
    /// repoints an open pin of the local image to the new cloud revision.
    /// Called by `upload_round` after each successful upload.
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
