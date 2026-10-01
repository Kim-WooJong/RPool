//! Shared immutable revision exchange. Incoming writes are only materialized
//! while unmounted and with no persistent VFS files awaiting recovery.
use super::*;
use crate::mount::shared_model::{self, Content, Event, Resolved};
use crate::mount::shared_transport::SharedTransport;

#[cfg(test)]
#[path = "workspace_shared_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SharedState {
    root: String,
    worker: String,
    device: String,
    events: BTreeMap<String, Event>,
    published: BTreeSet<String>,
    /// Revision actually materialized/edited at each local path, NOT the latest
    /// remote head. Offline edits must retain their real ancestor.
    bases: BTreeMap<String, String>,
    #[serde(default)]
    tombstones: BTreeMap<String, Vec<String>>,
}

impl Workspace {
    pub(crate) fn is_shared(&self) -> bool {
        self.catalog.shared.is_some()
    }

    pub(crate) fn configure_shared(
        &mut self,
        root: Option<&str>,
        worker: Option<&str>,
    ) -> Result<()> {
        self.recover_shared_apply()?;
        let (root, worker) = match (root, worker) {
            (None, None) if self.catalog.shared.is_none() => return Ok(()),
            (Some(root), Some(worker)) if !root.trim().is_empty() && !worker.trim().is_empty() => {
                (root.trim(), worker.trim())
            }
            _ => bail!(
                "shared workspaces require both --shared-root and --worker-name on every start"
            ),
        };
        if worker.chars().count() > 64 || worker.chars().any(char::is_control) {
            bail!("worker name must be 1–64 printable characters");
        }
        // Validate before persisting a binding; a typo must not poison a new
        // workspace. Encryption policy is additionally enforced on network I/O.
        let _ = SharedTransport::new(&self.rclone, root)?;
        if let Some(saved) = &self.catalog.shared {
            if saved.root != root {
                bail!("workspace is bound to another shared root; use a new workspace");
            }
        }
        let mut next = self.catalog.clone();
        // Older binaries only accept v1 and must refuse, not silently drop the
        // shared ancestry when saving this catalog.
        next.version = 2;
        if let Some(shared) = &mut next.shared {
            shared.worker = worker.into();
        } else {
            let mut entropy = [0u8; 24];
            getrandom::fill(&mut entropy)
                .map_err(|e| anyhow!("cannot create device identity: {e}"))?;
            next.shared = Some(SharedState {
                root: root.into(),
                worker: worker.into(),
                device: blake3::hash(&entropy).to_hex().to_string(),
                events: BTreeMap::new(),
                published: BTreeSet::new(),
                bases: BTreeMap::new(),
                tombstones: BTreeMap::new(),
            });
        }
        self.checkpoint(next)
    }

    /// Events and the local base advance in one catalog checkpoint BEFORE any
    /// network operation. Lost acknowledgements retry exactly the same object.
    fn record_shared_changes(&mut self) -> Result<()> {
        let mut next = self.catalog.clone();
        let Some(shared) = &mut next.shared else {
            return Ok(());
        };
        for (name, entry) in &next.entries {
            let previous = shared.bases.get(name).and_then(|id| shared.events.get(id));
            if entry.deleted && previous.is_none() {
                continue;
            }
            let same = match previous {
                Some(event) => match &event.content {
                    Some(content) => {
                        !entry.deleted
                            && content.hash == entry.hash
                            && content.size == entry.size
                            && entry.manifest
                                == format!(
                                    "{}.json",
                                    crate::manifest::manifest_fingerprint(&content.manifest)?
                                )
                    }
                    None => entry.deleted,
                },
                None => false,
            };
            if same {
                continue;
            }
            let content = if entry.deleted {
                None
            } else {
                validate_component(&entry.manifest)?;
                let manifest: Manifest =
                    read_json(&self.metadata.join("archives").join(&entry.manifest))?;
                Some(Content {
                    hash: entry.hash.clone(),
                    size: entry.size,
                    manifest,
                })
            };
            let event = Event {
                version: 1,
                worker: shared.worker.clone(),
                device: shared.device.clone(),
                path: previous
                    .map(|e| e.path.clone())
                    .unwrap_or_else(|| name.clone()),
                parents: shared
                    .bases
                    .get(name)
                    .cloned()
                    .map(|id| vec![id])
                    .unwrap_or_else(|| shared.tombstones.get(name).cloned().unwrap_or_default()),
                content,
            };
            event.validate()?;
            let id = event.id()?;
            shared.events.insert(id.clone(), event);
            shared.bases.insert(name.clone(), id);
        }
        self.checkpoint(next)
    }

    pub(crate) fn sync_shared(&mut self, materialize: bool) -> Result<()> {
        if !self.is_shared() {
            return Ok(());
        }
        self.record_shared_changes()?;
        let shared = self.catalog.shared.as_ref().unwrap();
        let transport = SharedTransport::new(&self.rclone, &shared.root)?;
        let pending: Vec<_> = shared
            .events
            .iter()
            .filter(|(id, _)| !shared.published.contains(*id))
            .map(|(id, e)| Ok((id.clone(), serde_json::to_vec(e)?)))
            .collect::<Result<_>>()?;
        for (id, bytes) in pending {
            transport.publish(&id, &bytes)?;
            let mut next = self.catalog.clone();
            next.shared.as_mut().unwrap().published.insert(id);
            self.checkpoint(next)?;
        }
        let known = self
            .catalog
            .shared
            .as_ref()
            .unwrap()
            .events
            .keys()
            .cloned()
            .collect();
        let incoming = transport.list_missing(&known)?;
        let mut next = self.catalog.clone();
        let shared = next.shared.as_mut().unwrap();
        for (id, bytes) in incoming {
            let event: Event = serde_json::from_slice(&bytes).context("invalid shared revision")?;
            event.validate()?;
            if event.id()? != id {
                bail!("shared revision identity mismatch");
            }
            shared.events.insert(id.clone(), event);
            shared.published.insert(id);
        }
        // Missing parents, bad paths, or malformed events fail closed. Never
        // turn a partial listing into a deletion of already-known revisions.
        let desired = shared_model::reduce(&shared.events)?;
        self.checkpoint(next)?;
        println!(
            "Shared file list: {} files, {} retained revisions",
            desired.len(),
            self.catalog.shared.as_ref().unwrap().events.len()
        );
        if !materialize {
            println!(
                "Incoming shared files/deletions deferred until safe unmounted sync or next start."
            );
            return Ok(());
        }
        if !self.shared_materialization_safe()? {
            println!("Incoming shared files deferred: persistent VFS cache or mount lease remains. Recover with the original mount; never delete the cache.");
            return Ok(());
        }
        self.materialize_shared(desired, |rclone, manifest, output, policy| {
            crate::commands::get(
                rclone,
                &manifest.to_string_lossy(),
                output,
                policy.workers,
                policy.retries,
            )
        })
    }

    pub(super) fn shared_materialization_safe(&self) -> Result<bool> {
        if self.metadata.join("mount-process.json").exists() {
            return Ok(false);
        }
        let cache = self
            .files
            .parent()
            .context("workspace parent missing")?
            .join("vfs-cache");
        fn has_files(dir: &Path) -> Result<bool> {
            require_directory(dir)?;
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                let meta = fs::symlink_metadata(&path)?;
                if !meta.is_dir()
                    || meta.file_type().is_symlink()
                    || is_reparse(&meta)
                    || has_files(&path)?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Ok(!cache.exists() || !has_files(&cache)?)
    }

    fn materialize_shared(
        &mut self,
        desired: BTreeMap<String, Resolved>,
        restore: impl Fn(&str, &Path, &Path, &PoolDefinition) -> Result<()>,
    ) -> Result<()> {
        // Complete scan before changing anything; new local edits not yet
        // archived must not be overwritten by a remote result.
        let (current, _) = scan(&self.files)?;
        for (name, file) in &current {
            if !self
                .catalog
                .entries
                .get(name)
                .is_some_and(|e| !e.deleted && e.hash == file.hash && e.size == file.size)
            {
                bail!("local files changed during shared sync; retry to archive them first");
            }
        }
        for (name, entry) in &self.catalog.entries {
            if !entry.deleted && !current.contains_key(name) {
                bail!("local deletion during shared sync; retry before applying incoming files");
            }
        }
        // Download and verify every desired revision before touching the tree.
        let stage = tempfile::Builder::new()
            .prefix("shared-stage-")
            .tempdir_in(&self.metadata)?;
        for (name, resolved) in &desired {
            validate_relative(name)?;
            let content = resolved
                .event
                .content
                .as_ref()
                .context("resolved deletion has content path")?;
            if current
                .get(name)
                .is_some_and(|f| f.hash == content.hash && f.size == content.size)
            {
                continue;
            }
            let manifest_path = stage.path().join(format!("{}.json", resolved.event_id));
            atomic_json(&manifest_path, &content.manifest)?;
            let output = stage.path().join(&resolved.event_id);
            if !output.exists() {
                restore(&self.rclone, &manifest_path, &output, &self.catalog.policy)?;
            }
            require_regular(&output)?;
            if fs::metadata(&output)?.len() != content.size
                || hash_file_range(&output, 0, content.size)? != content.hash
            {
                bail!("shared download failed content verification");
            }
            crate::utils::sync_file(&output)?;
        }
        // Backups survive crashes and open external file handles. Never unlink
        // displaced contents; an interrupted pass is conservatively re-uploaded
        // on restart rather than treating the absent file as safe to discard.
        let recovery_root = self.metadata.join("shared-recovery");
        fs::create_dir_all(&recovery_root)?;
        require_directory(&recovery_root)?;
        sync_directory(&self.metadata)?;
        let recovery = tempfile::Builder::new()
            .prefix("apply-")
            .tempdir_in(&recovery_root)?
            .keep();
        sync_directory(&recovery_root)?;
        let recovery_name = recovery.file_name().unwrap().to_string_lossy().to_string();
        atomic_json(&self.metadata.join("shared-apply.json"), &recovery_name)?;
        for (name, old) in &current {
            if desired.get(name).is_some_and(|r| {
                r.event
                    .content
                    .as_ref()
                    .is_some_and(|c| c.hash == old.hash && c.size == old.size)
            }) {
                continue;
            }
            let path = self.files.join(name);
            require_regular(&path)?;
            if fs::metadata(&path)?.len() != old.size
                || hash_file_range(&path, 0, old.size)? != old.hash
            {
                bail!("local edit during reconciliation; retry to preserve its revision");
            }
            let backup = tempfile::Builder::new()
                .prefix("version-")
                .tempdir_in(&recovery)?
                .keep();
            sync_directory(&recovery)?;
            atomic_json(&backup.join("origin.json"), name)?;
            fs::rename(&path, backup.join("content"))?;
            sync_directory(&backup)?;
            sync_directory(path.parent().unwrap())?;
            if hash_file_range(
                &backup.join("content"),
                0,
                fs::metadata(backup.join("content"))?.len(),
            )? != old.hash
            {
                // No overwriting a racing recreation at the source path.
                let _ = fs::hard_link(backup.join("content"), &path);
                bail!("racing edit preserved in shared-recovery; reconcile after closing editors");
            }
        }
        let mut next = self.catalog.clone();
        for entry in next.entries.values_mut() {
            entry.deleted = true;
        }
        next.shared.as_mut().unwrap().bases.clear();
        let shared = next.shared.as_mut().unwrap();
        shared.tombstones.clear();
        let superseded: BTreeSet<_> = shared
            .events
            .values()
            .flat_map(|e| e.parents.iter().cloned())
            .collect();
        for (id, event) in &shared.events {
            if event.content.is_none() && !superseded.contains(id) {
                shared
                    .tombstones
                    .entry(event.path.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        for (name, resolved) in &desired {
            let content = resolved.event.content.as_ref().unwrap();
            let target = self.files.join(name);
            self.shared_parent_directories(name)?;
            if target.is_dir() {
                remove_empty_tree(&target)?;
            }
            if !target.exists() {
                fs::hard_link(stage.path().join(&resolved.event_id), &target)?;
                sync_directory(target.parent().unwrap())?;
            }
            require_regular(&target)?;
            if fs::metadata(&target)?.len() != content.size
                || hash_file_range(&target, 0, content.size)? != content.hash
            {
                bail!("local collision while installing shared file; both revisions retained");
            }
            let archive = format!(
                "{}.json",
                crate::manifest::manifest_fingerprint(&content.manifest)?
            );
            atomic_json(
                &self.metadata.join("archives").join(&archive),
                &content.manifest,
            )?;
            next.entries.insert(
                name.clone(),
                Entry {
                    hash: content.hash.clone(),
                    size: content.size,
                    manifest: archive,
                    deleted: false,
                },
            );
            next.shared
                .as_mut()
                .unwrap()
                .bases
                .insert(name.clone(), resolved.event_id.clone());
        }
        next.directories = scan(&self.files)?.1;
        self.checkpoint(next)?;
        fs::remove_file(self.metadata.join("shared-apply.json"))?;
        sync_directory(&self.metadata)?;
        println!("Shared reconciliation complete: {} files; displaced local versions retained in .rpool/shared-recovery", desired.len());
        Ok(())
    }

    fn shared_parent_directories(&self, name: &str) -> Result<()> {
        let mut dir = self.files.clone();
        let components: Vec<_> = name.split('/').collect();
        for component in &components[..components.len() - 1] {
            let parent = dir.clone();
            dir.push(component);
            if !dir.exists() {
                fs::create_dir(&dir)?;
                sync_directory(&parent)?;
            }
            require_directory(&dir)?;
        }
        Ok(())
    }

    fn recover_shared_apply(&self) -> Result<()> {
        let journal = self.metadata.join("shared-apply.json");
        if !journal.exists() {
            return Ok(());
        }
        if !self.shared_materialization_safe()? {
            bail!("interrupted shared reconciliation requires an unmounted workspace with drained cache");
        }
        require_regular(&journal)?;
        let name: String = read_json(&journal)?;
        validate_component(&name)?;
        let root = self.metadata.join("shared-recovery").join(name);
        require_directory(&root)?;
        for item in fs::read_dir(&root)? {
            let dir = item?.path();
            require_directory(&dir)?;
            let content = dir.join("content");
            if !content.exists() {
                continue;
            }
            require_regular(&content)?;
            let origin: String = read_json(&dir.join("origin.json"))?;
            validate_relative(&origin)?;
            self.shared_parent_directories(&origin)?;
            let target = self.files.join(&origin);
            if !target.exists() {
                fs::hard_link(&content, &target)?;
                sync_directory(target.parent().unwrap())?;
            } else {
                require_regular(&target)?;
                let size = fs::metadata(&content)?.len();
                let hash = hash_file_range(&content, 0, size)?;
                if fs::metadata(&target)?.len() == size
                    && hash_file_range(&target, 0, size)? == hash
                {
                    continue;
                }
                // Crash may have installed the new version already. Restore the
                // previous bytes alongside it, never over it.
                let recovered = self.files.join(format!(
                    "recovered-{}.bin",
                    blake3::hash(format!("{origin}\0{hash}").as_bytes()).to_hex()
                ));
                if !recovered.exists() {
                    fs::hard_link(&content, &recovered)?;
                } else if fs::metadata(&recovered)?.len() != size
                    || hash_file_range(&recovered, 0, size)? != hash
                {
                    bail!("recovery filename occupied; preserved backup needs manual recovery");
                }
                sync_directory(recovered.parent().unwrap())?;
            }
        }
        sync_directory(&self.files)?;
        fs::remove_file(journal)?;
        sync_directory(&self.metadata)?;
        println!("Interrupted shared apply recovered; previous bytes restored without overwriting newer files.");
        Ok(())
    }
}

fn remove_empty_tree(dir: &Path) -> Result<()> {
    require_directory(dir)?;
    for item in fs::read_dir(dir)? {
        // Refuse rather than deleting any concurrently created file.
        remove_empty_tree(&item?.path())?;
    }
    fs::remove_dir(dir)?;
    sync_directory(dir.parent().context("directory parent missing")?)
}
