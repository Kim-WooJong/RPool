//! Latest-cloud-wins shared drive. A proposal is not a commit acknowledgement.
use super::namespace::{durable_json, CheckpointCursor, Intent};
use super::shared_checkpoint_model::{Checkpoint, ManagedContent, Proposal, Version};
use super::shared_checkpoint_transport::{self as transport, CheckpointIo, Pointer, RcloneIo};
use super::shared_model::{Content, Event};
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

#[derive(Serialize, Deserialize)]
struct UploadLedger {
    version: u32,
    archive: String,
    attempt: String,
    serial: u64,
    objects: Vec<String>,
    finished: bool,
}
fn metadata_objects(manifest: &Manifest, remotes: &[String]) -> Vec<String> {
    let mut objects: BTreeSet<_> = manifest.shards.iter().map(|s| s.object.clone()).collect();
    objects.extend(
        remotes.iter().map(|r| {
            crate::utils::remote_join(r, &format!("{}/manifest.json", manifest.archive_id))
        }),
    );
    objects.into_iter().collect()
}
fn archive_id(root_hash: &str, serial: u64, device: &str, intent: &str) -> String {
    format!("epoch-{root_hash}-{serial}-{device}-{intent}")
}
fn ledger_key(archive: &str, objects: &[String], attempt: &str) -> Result<String> {
    let hash = blake3::hash(&serde_json::to_vec(&(archive, objects, attempt))?)
        .to_hex()
        .to_string();
    Ok(format!("uploads/{hash}.json"))
}
fn validate_ledger(ledger: &UploadLedger, root_hash: &str) -> Result<()> {
    if ledger.attempt.len() != 64 || !ledger.attempt.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid upload attempt");
    }
    if ledger.version != 5
        || !ledger
            .archive
            .starts_with(&format!("epoch-{root_hash}-{}-", ledger.serial))
    {
        bail!("upload ledger root/serial mismatch");
    }
    let parts: Vec<_> = ledger.archive.split('-').collect();
    if parts.len() != 5
        || parts[3..]
            .iter()
            .any(|s| s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        bail!("invalid upload ledger identity");
    }
    for object in &ledger.objects {
        let (remote, path) = object.split_once(':').context("invalid ledger object")?;
        if remote.is_empty()
            || object.contains('\\')
            || object.chars().any(char::is_control)
            || !path.split('/').any(|p| p == ledger.archive)
            || path.split('/').any(|p| matches!(p, "." | ".."))
        {
            bail!("upload ledger contains an unowned object");
        }
    }
    Ok(())
}
/// Incomplete old uploads remain discoverable and are revisited: a paused writer
/// can finish after the first sweep. Finished records can be removed permanently.
fn sweep_uploads(io: &dyn CheckpointIo, root_hash: &str, checkpoint: &Checkpoint) -> Result<()> {
    let versions: Vec<_> = checkpoint
        .files
        .values()
        .chain(checkpoint.history.values().flatten())
        .collect();
    let protected_ids: BTreeSet<_> = versions
        .iter()
        .map(|v| v.value.content.manifest.archive_id.as_str())
        .collect();
    let protected: BTreeSet<_> = versions
        .iter()
        .flat_map(|v| {
            v.value
                .objects
                .iter()
                .chain(v.value.content.manifest.shards.iter().map(|s| &s.object))
        })
        .collect();
    for name in io.list("uploads")? {
        if name.contains('/') || !name.ends_with(".json") {
            bail!("invalid upload ledger filename");
        }
        let key = format!("uploads/{name}");
        let Some(bytes) = io.read(&key)? else {
            continue;
        };
        let ledger: UploadLedger = serde_json::from_slice(&bytes)?;
        validate_ledger(&ledger, root_hash)?;
        if ledger_key(&ledger.archive, &ledger.objects, &ledger.attempt)? != key {
            bail!("upload ledger identity mismatch");
        }
        if ledger.serial >= checkpoint.serial {
            continue;
        }
        if protected_ids.contains(ledger.archive.as_str()) {
            continue;
        }
        {
            for object in &ledger.objects {
                if !protected.contains(object) {
                    io.remove_object(object)?;
                }
            }
        }
        // If accepted, checkpoint already owns its references. If obsolete,
        // all exact objects were deleted. Unknown outcome leaves this record.
        if ledger.finished {
            io.remove(&key)?;
        }
    }
    Ok(())
}
impl VirtualDrive {
    pub(crate) fn isolate_previous_native_cache(&self) -> Result<()> {
        super::adapter::preflight_virtual(&self.root)?;
        let cache = self.root.join("vfs-cache");
        if !cache.exists() {
            return Ok(());
        }
        super::retention::real_tree(&cache)?;
        if fs::read_dir(&cache)?.next().is_none() {
            return Ok(());
        }
        // Native cached writes have no reliable revision stamp. Never replay
        // an old mount's cache against a newer cloud checkpoint.
        let recovery = self.root.join("recovered-native-cache");
        fs::create_dir_all(&recovery)?;
        super::retention::real_tree(&recovery)?;
        let target = recovery.join(super::namespace::random_id()?);
        fs::rename(&cache, &target)?;
        #[cfg(unix)]
        {
            File::open(&recovery)?.sync_all()?;
            File::open(&self.root)?.sync_all()?;
        }
        eprintln!("Previous native VFS cache isolated without deletion at {}; inspect for unsaved files before removing manually", target.display());
        Ok(())
    }
    fn checkpoint_io(&self) -> Result<RcloneIo> {
        RcloneIo::new(
            &self.rclone,
            self.shared_root
                .as_deref()
                .context("bounded shared root missing")?,
        )
    }
    pub(crate) fn pull_checkpoint(&self) -> Result<()> {
        let io = self.checkpoint_io()?;
        let (pointer, checkpoint) =
            transport::load(&io)?.context("shared coordinator has not initialized this root")?;
        self.install_checkpoint(&pointer, &checkpoint)
    }
    pub(crate) fn sync_checkpoint(&self) -> Result<()> {
        let _gate = self.sync_gate.lock().unwrap();
        // Drain the finite queue present at entry when this PC can acknowledge it.
        // Workers stop after publication and wait for the designated coordinator.
        let passes = if self.checkpoint_coordinator {
            self.state.lock().unwrap().pending.len().saturating_add(1)
        } else {
            1
        };
        for _ in 0..passes {
            self.sync_checkpoint_pass()?;
            if self.state.lock().unwrap().pending.is_empty() {
                break;
            }
        }
        let remaining = self.state.lock().unwrap().pending.len();
        if remaining > 0 {
            eprintln!("Shared writes awaiting coordinator acknowledgement: {remaining}; local spool retained");
        }
        Ok(())
    }
    fn validate_checkpoint_high_water(&self, io: &dyn CheckpointIo) -> Result<()> {
        // Check local high-water BEFORE any coordinator publication or deletion.
        if let Some(cursor) = &self.state.lock().unwrap().checkpoint {
            let (remote, _) = transport::load(io)?
                .context("active checkpoint missing; refusing reinitialization")?;
            if remote.owner != cursor.owner
                || remote.serial < cursor.serial
                || (remote.serial == cursor.serial && remote.hash != cursor.hash)
            {
                bail!("remote checkpoint rollback/owner change; refusing coordination");
            }
        }
        Ok(())
    }
    fn sync_checkpoint_pass(&self) -> Result<()> {
        let io = self.checkpoint_io()?;
        self.validate_checkpoint_high_water(&io)?;
        let device = self.state.lock().unwrap().device.clone();
        let initial = if self.checkpoint_coordinator {
            let state = self.state.lock().unwrap();
            let mut seed = Checkpoint::empty(&device);
            if state.checkpoint.is_none() {
                for (path, resolved) in state.resolved()? {
                    if let Some(content) = resolved.event.content {
                        seed.files.insert(
                            path,
                            Version {
                                id: resolved.event_id,
                                value: ManagedContent {
                                    content,
                                    objects: vec![],
                                },
                            },
                        );
                    }
                }
            }
            Some(seed)
        } else {
            None
        };
        let floor = self.state.lock().unwrap().checkpoint.clone();
        let (pointer, checkpoint) = if self.checkpoint_coordinator {
            transport::coordinate(
                &io,
                &self.root,
                &device,
                self.checkpoint_keep,
                initial,
                floor.as_ref(),
            )?
        } else {
            transport::load(&io)?
                .context("start the designated coordinator before shared synchronization")?
        };
        self.install_checkpoint(&pointer, &checkpoint)?;
        // One durable local operation per pass preserves local MOVE/write/delete order.
        let pending = self.state.lock().unwrap().pending.first().cloned();
        if let Some(intent) = pending {
            if intent.depends_on.is_some() {
                bail!("MOVE prerequisite has not been acknowledged; source retained");
            }
            let proposal_path = self
                .root
                .join("spool")
                .join(&intent.id)
                .join(format!("proposal-{}.json", pointer.serial));
            let proposal: Proposal = if proposal_path.exists() {
                crate::utils::read_json(&proposal_path)?
            } else {
                let value = if intent.spool.is_some() {
                    let source = self.spool_path(&intent);
                    if fs::metadata(&source)?.len() != intent.size
                        || crate::utils::hash_file_range(&source, 0, intent.size)? != intent.hash
                    {
                        bail!("pending checkpoint spool integrity failure");
                    }
                    let staged_dir = source
                        .parent()
                        .unwrap()
                        .join(format!("epoch-{}", pointer.serial));
                    fs::create_dir_all(&staged_dir)?;
                    let staged = staged_dir.join(
                        Path::new(&intent.path)
                            .file_name()
                            .context("filename missing")?,
                    );
                    if !staged.exists() {
                        fs::hard_link(&source, &staged)?;
                    }
                    let archive = archive_id(&io.root_hash(), pointer.serial, &device, &intent.id);
                    let mut ledgers = vec![];
                    let (manifest, remotes) = super::workspace::upload_eligible_registered(
                        &self.rclone,
                        &self.policy,
                        &self.pool,
                        &staged,
                        &archive,
                        &mut |objects| {
                            // A resumed stale transaction must never start more writes.
                            let current =
                                transport::load(&io)?.context("checkpoint disappeared")?.0;
                            if current != pointer {
                                bail!("checkpoint advanced before upload; local bytes retained");
                            }
                            let objects: Vec<_> = objects
                                .iter()
                                .cloned()
                                .collect::<BTreeSet<_>>()
                                .into_iter()
                                .collect();
                            // Every attempt has a separate unfinished ledger. A paused
                            // retry cannot be hidden by a previous attempt's finished flag.
                            let attempt = super::namespace::random_id()?;
                            let key = ledger_key(&archive, &objects, &attempt)?;
                            let ledger = UploadLedger {
                                version: 5,
                                archive: archive.clone(),
                                attempt,
                                serial: pointer.serial,
                                objects,
                                finished: false,
                            };
                            io.put(&key, &serde_json::to_vec(&ledger)?)?;
                            ledgers.push((key, ledger));
                            Ok(())
                        },
                    )?;
                    for (key, mut ledger) in ledgers {
                        ledger.finished = true;
                        io.put(&key, &serde_json::to_vec(&ledger)?)?;
                    }
                    Some(ManagedContent {
                        objects: metadata_objects(&manifest, &remotes),
                        content: Content {
                            hash: intent.hash.clone(),
                            size: intent.size,
                            manifest,
                        },
                    })
                } else {
                    None
                };
                let proposal = Proposal {
                    version: 5,
                    id: intent.id.clone(),
                    serial: pointer.serial,
                    device: device.clone(),
                    worker: self.state.lock().unwrap().worker.clone(),
                    path: intent.path.clone(),
                    base: intent.checkpoint_base.clone(),
                    value,
                };
                proposal.validate()?;
                durable_json(&proposal_path, &proposal)?;
                proposal
            };
            if proposal.id != intent.id
                || proposal.serial != pointer.serial
                || proposal.device != device
                || proposal.path != intent.path
                || proposal.base != intent.checkpoint_base
                || proposal
                    .value
                    .as_ref()
                    .map(|v| (&v.content.hash, v.content.size))
                    != intent.spool.as_ref().map(|_| (&intent.hash, intent.size))
            {
                bail!("saved proposal does not match local intent");
            }
            transport::publish_proposal(&io, &proposal)?;
            // DO NOT commit/remove local intent here. Immutable publication is not acceptance.
        }
        let floor = self.state.lock().unwrap().checkpoint.clone();
        let (pointer, checkpoint) = if self.checkpoint_coordinator {
            transport::coordinate(
                &io,
                &self.root,
                &device,
                self.checkpoint_keep,
                None,
                floor.as_ref(),
            )?
        } else {
            transport::load(&io)?.context("checkpoint disappeared")?
        };
        self.install_checkpoint(&pointer, &checkpoint)?;
        self.cleanup_checkpoint_spool()?;
        if self.checkpoint_coordinator {
            sweep_uploads(&io, &io.root_hash(), &checkpoint)?;
        }
        Ok(())
    }
    pub(crate) fn install_checkpoint(
        &self,
        pointer: &Pointer,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        checkpoint.validate()?;
        let bytes = serde_json::to_vec(checkpoint)?;
        if pointer.hash != blake3::hash(&bytes).to_hex().as_str()
            || pointer.owner != checkpoint.owner
            || pointer.serial != checkpoint.serial
        {
            bail!("checkpoint install identity mismatch");
        }
        let mut state = self.state.lock().unwrap();
        if let Some(old) = &state.checkpoint {
            if old.owner != pointer.owner
                || old.serial > pointer.serial
                || (old.serial == pointer.serial && old.hash != pointer.hash)
            {
                bail!("checkpoint rollback/owner change detected; local data retained");
            }
        }
        let accepted: BTreeSet<_> = checkpoint
            .receipts
            .iter()
            .cloned()
            .chain(
                checkpoint
                    .files
                    .values()
                    .chain(checkpoint.history.values().flatten())
                    .map(|v| v.id.clone()),
            )
            .collect();
        let mut next = state.clone();
        next.version = 5;
        next.pending.clear();
        let mut pins = self.pins.lock().unwrap();
        let advanced = state
            .checkpoint
            .as_ref()
            .is_none_or(|c| c.hash != pointer.hash);
        if advanced {
            pins.retain(|_, r| matches!(r, super::virtual_drive::Revision::Local { .. }));
        }
        let local_ids: BTreeSet<_> = state.pending.iter().map(|i| i.id.clone()).collect();
        let own_advance = state
            .checkpoint
            .as_ref()
            .is_some_and(|old| old.serial.checked_add(1) == Some(pointer.serial))
            && !checkpoint.receipts.is_empty()
            && checkpoint.receipts.iter().all(|id| local_ids.contains(id));
        let mut surviving = BTreeSet::new();
        for intent in &state.pending {
            if accepted.contains(&intent.id) {
                next.accepted_spool
                    .insert(intent.id.clone(), intent.clone());
                if pins.get(&intent.path).is_some_and(|r| r.id() == intent.id) {
                    pins.remove(&intent.path);
                }
                continue;
            }
            let prerequisite_failed = intent
                .depends_on
                .as_ref()
                .is_some_and(|id| !accepted.contains(id) && !surviving.contains(id));
            if prerequisite_failed {
                self.export_stale_intent(intent)?;
                next.accepted_spool
                    .insert(intent.id.clone(), intent.clone());
                if pins.get(&intent.path).is_some_and(|r| r.id() == intent.id) {
                    pins.remove(&intent.path);
                }
                continue;
            }
            {
                // A fresh generic PUT can still carry an old editor's base.
                // Reject it locally even when its request serial is current.
                let current = checkpoint.files.get(&intent.path).map(|v| v.id.as_str());
                let unchanged = intent
                    .checkpoint_base
                    .as_deref()
                    .is_some_and(|base| current == Some(base))
                    || ((own_advance || intent.checkpoint_serial == Some(pointer.serial))
                        && intent.checkpoint_base.is_none()
                        && current.is_none());
                let dependency = intent.checkpoint_base.as_ref().is_some_and(|base| {
                    next.pending.iter().any(|prior| {
                        &prior.id == base && prior.path == intent.path && prior.spool.is_some()
                    })
                });
                if !unchanged && !dependency {
                    self.export_stale_intent(intent)?;
                    // The export is durable BEFORE forgetting the old namespace operation.
                    next.accepted_spool
                        .insert(intent.id.clone(), intent.clone());
                    if pins.get(&intent.path).is_some_and(|r| r.id() == intent.id) {
                        pins.remove(&intent.path);
                    }
                    continue;
                }
            }
            let mut intent = intent.clone();
            if intent
                .depends_on
                .as_ref()
                .is_some_and(|id| accepted.contains(id))
            {
                intent.depends_on = None;
            }
            intent.checkpoint_serial = Some(pointer.serial);
            surviving.insert(intent.id.clone());
            next.pending.push(intent);
        }
        let own_accepted: BTreeSet<_> = state
            .pending
            .iter()
            .filter(|i| accepted.contains(&i.id))
            .map(|i| i.id.clone())
            .collect();
        // A positively acknowledged local deletion permits a deliberate new
        // create at that path. Remote deletions keep old editor baselines fenced.
        for intent in &state.pending {
            if own_accepted.contains(&intent.id)
                && intent.spool.is_none()
                && !checkpoint.files.contains_key(&intent.path)
            {
                next.bases.remove(&intent.path);
            }
        }
        next.events.clear();
        next.published.clear();
        next.committed_intents.clear();
        for (path, version) in &checkpoint.files {
            let event = Event {
                version: 1,
                worker: "checkpoint".into(),
                device: version.id.clone(),
                path: path.clone(),
                parents: vec![],
                content: Some(version.value.content.clone()),
            };
            let id = event.id()?;
            if own_accepted.contains(&version.id) {
                next.bases.insert(path.clone(), vec![id.clone()]);
            }
            next.checkpoint_ids.insert(id.clone(), version.id.clone());
            next.events.insert(id, event);
        }
        // Preserve last-read baselines, but never old cloud pins. Explicit reads
        // and positive own acknowledgements advance an editor baseline.
        let referenced: BTreeSet<_> = next
            .bases
            .values()
            .flatten()
            .cloned()
            .chain(next.events.keys().cloned())
            .collect();
        next.checkpoint_ids.retain(|id, _| referenced.contains(id));
        next.checkpoint = Some(CheckpointCursor {
            owner: pointer.owner.clone(),
            serial: pointer.serial,
            hash: pointer.hash.clone(),
        });
        next.save(&self.root)?;
        *state = next;
        Ok(())
    }
    fn export_stale_intent(&self, intent: &Intent) -> Result<()> {
        let output = self.root.join("recovered-writes");
        fs::create_dir_all(&output)?;
        if fs::symlink_metadata(&output)?.file_type().is_symlink() {
            bail!("recovery directory must not be a symlink");
        }
        if intent.spool.is_some() {
            let source = self.spool_path(intent);
            let target = output.join(format!("{}.stale.bin", intent.id));
            if target.exists() {
                if fs::metadata(&target)?.len() != intent.size
                    || crate::utils::hash_file_range(&target, 0, intent.size)? != intent.hash
                {
                    bail!("stale recovery collision");
                }
            } else {
                let mut temp = tempfile::NamedTempFile::new_in(&output)?;
                std::io::copy(&mut File::open(&source)?, &mut temp)?;
                temp.as_file().sync_all()?;
                if temp.as_file().metadata()?.len() != intent.size
                    || crate::utils::hash_file_range(temp.path(), 0, intent.size)? != intent.hash
                {
                    bail!("stale recovery integrity failure");
                }
                temp.persist_noclobber(target).map_err(|e| e.error)?;
            }
        }
        durable_json(&output.join(format!("{}.stale.json", intent.id)), intent)?;
        #[cfg(unix)]
        File::open(output)?.sync_all()?;
        eprintln!("Stale local operation for {} isolated in recovered-writes; latest cloud state retained", intent.path);
        Ok(())
    }
    pub(crate) fn cleanup_checkpoint_spool(&self) -> Result<u64> {
        let mut state = self.state.lock().unwrap();
        state.save(&self.root)?;
        let leases = self.local_leases.lock().unwrap();
        let mut next = state.clone();
        let mut bytes = 0u64;
        for (id, intent) in &state.accepted_spool {
            if state.pending.iter().any(|i| &i.id == id)
                || leases.get(id).is_some_and(|l| l.strong_count() > 0)
            {
                continue;
            }
            let dir = self.root.join("spool").join(id);
            if dir.exists() {
                super::retention::real_tree(&dir)?;
                if intent.spool.is_some() {
                    let source = dir.join("content");
                    if !fs::metadata(&source).is_ok_and(|m| m.len() == intent.size)
                        || !crate::utils::hash_file_range(&source, 0, intent.size)
                            .is_ok_and(|h| h == intent.hash)
                    {
                        continue;
                    }
                    bytes = bytes.saturating_add(intent.size);
                }
                fs::remove_dir_all(&dir)?;
            }
            next.accepted_spool.remove(id);
        }
        #[cfg(unix)]
        File::open(self.root.join("spool"))?.sync_all()?;
        next.save(&self.root)?;
        *state = next;
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "shared_checkpoint_tests.rs"]
mod tests;
