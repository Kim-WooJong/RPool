use super::capacity::CapacityStatus;
use super::namespace::{durable_json, random_id, valid_path, Intent, Namespace};
use super::shard_cache::ShardCache;
use super::shared_model::{Content, Event};
use super::shared_transport::SharedTransport;
use crate::prelude::*;

#[derive(Clone, Debug)]
pub(crate) enum Revision {
    Cloud {
        id: String,
        content: Content,
    },
    Local {
        id: String,
        path: PathBuf,
        size: u64,
        _lease: Arc<()>,
    },
}
impl Revision {
    pub fn id(&self) -> &str {
        match self {
            Self::Cloud { id, .. } | Self::Local { id, .. } => id,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::Cloud { content, .. } => content.size,
            Self::Local { size, .. } => *size,
        }
    }
}
pub(crate) struct VirtualDrive {
    pub root: PathBuf,
    pub state: Mutex<Namespace>,
    pub policy: PoolDefinition,
    pub pool: String,
    pub rclone: String,
    pub shared_root: Option<String>,
    pub cache: ShardCache,
    pub capacity: Mutex<Option<CapacityStatus>>,
    pub sync_gate: Mutex<()>,
    pub pins: Mutex<BTreeMap<String, Revision>>,
    pub local_leases: Mutex<BTreeMap<String, std::sync::Weak<()>>>,
    pub spool_limit: u64,
    pub spool_writes: Mutex<()>,
    _lock: File,
}
impl VirtualDrive {
    pub(crate) fn open(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        shared: Option<&str>,
        cache_limit: u64,
    ) -> Result<Self> {
        let shared_name = shared.map(|root| format!("{}/virtual-v3", root.trim_end_matches('/')));
        let shared = shared_name.as_deref();
        if root.join(".rpool/catalog.json").exists() {
            bail!(
                "Use a new virtual workspace; existing replica/cache is never silently converted"
            );
        }
        if !root.exists() {
            fs::create_dir_all(root)?;
        }
        if !root.join("virtual.json").exists() && fs::read_dir(root)?.next().is_some() {
            bail!("virtual workspace must initially be empty");
        }
        let root = root.canonicalize()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("virtual.lock"))?;
        lock.try_lock()
            .context("virtual workspace is already in use")?;
        let config = root.join("virtual.json");
        let existing = config.exists();
        #[derive(Serialize, Deserialize)]
        struct Binding {
            version: u32,
            pool: String,
            shared: Option<String>,
            policy: PoolDefinition,
        }
        let binding: Binding = if config.exists() {
            crate::utils::read_json(&config)?
        } else {
            let policy = crate::pool::load_pool_store()?
                .pools
                .get(pool)
                .cloned()
                .context("unknown pool")?;
            let b = Binding {
                version: 1,
                pool: pool.into(),
                shared: shared.map(str::to_owned),
                policy,
            };
            durable_json(&config, &b)?;
            b
        };
        if binding.version != 1 || binding.pool != pool || binding.shared.as_deref() != shared {
            bail!("virtual workspace pool/shared-root mismatch");
        }
        for name in ["spool", "clean-cache", "anchor", ".rpool"] {
            fs::create_dir_all(root.join(name))?;
            checked_directory(&root.join(name))?;
        }
        if let Some(root) = shared {
            SharedTransport::new(rclone, root)?;
        }
        if existing && !root.join("namespace.json").exists() {
            bail!("existing virtual workspace lost its primary namespace; preserve spool and use recovery");
        }
        let state = Namespace::load(&root, worker)?;
        let cache = ShardCache::new(root.join("clean-cache"), cache_limit)?;
        Ok(Self {
            root,
            state: Mutex::new(state),
            policy: binding.policy,
            pool: pool.into(),
            rclone: rclone.into(),
            shared_root: shared.map(str::to_owned),
            cache,
            capacity: Mutex::new(None),
            sync_gate: Mutex::new(()),
            pins: Mutex::new(BTreeMap::new()),
            local_leases: Mutex::new(BTreeMap::new()),
            spool_limit: 64 * 1073741824,
            spool_writes: Mutex::new(()),
            _lock: lock,
        })
    }
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
        for (name, pinned) in self.pins.lock().unwrap().iter() {
            if let Some(current) = result.get(name).cloned() {
                if current.id() != pinned.id() {
                    let alternate =
                        super::shared_model::conflict_path(name, "incoming", current.id(), 0)?;
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
    #[cfg(test)]
    pub(crate) fn begin(&self, path: &str) -> Result<Intent> {
        valid_path(path)?;
        let visible = self.view()?.get(path).cloned();
        self.begin_observed(path, visible.as_ref())
    }
    pub(crate) fn begin_observed(&self, path: &str, visible: Option<&Revision>) -> Result<Intent> {
        valid_path(path)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let parents = if let Some(base) = s.bases.get(path) {
            base.clone()
        } else {
            match visible {
                Some(Revision::Cloud { id, .. }) => vec![id.clone()],
                _ => s.base(path)?,
            }
        };
        s.bases
            .entry(path.into())
            .or_insert_with(|| parents.clone());
        let event_path = parents
            .first()
            .and_then(|id| s.events.get(id))
            .map(|e| e.path.clone())
            .unwrap_or_else(|| path.into());
        // No trusted editor revision arrives with a generic DAV PUT. Never infer
        // a dependency on a newly arrived local/remote write that was not read.
        let depends_on = None;
        s.save(&self.root)?;
        let id = random_id()?;
        let dir = self.root.join("spool").join(&id);
        fs::create_dir(&dir)?;
        Ok(Intent {
            id: id.clone(),
            path: path.into(),
            event_path,
            parents,
            spool: Some(id),
            size: 0,
            hash: String::new(),
            depends_on,
        })
    }
    fn prepare_seal(&self, mut intent: Intent) -> Result<Intent> {
        let path = self.spool_path(&intent);
        let file = File::open(&path)?;
        file.sync_all()?;
        intent.size = file.metadata()?.len();
        intent.hash = crate::utils::hash_file_range(&path, 0, intent.size)?;
        durable_json(&path.parent().unwrap().join("intent.json"), &intent)?;
        #[cfg(unix)]
        {
            File::open(path.parent().unwrap())?.sync_all()?;
            File::open(self.root.join("spool"))?.sync_all()?;
        }
        Ok(intent)
    }
    pub(crate) fn seal(&self, intent: Intent) -> Result<()> {
        let intent = self.prepare_seal(intent)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut next = s.clone();
        next.pending.push(intent.clone());
        next.save(&self.root)?;
        *s = next;
        self.pins.lock().unwrap().insert(
            intent.path.clone(),
            Revision::Local {
                id: intent.id.clone(),
                path: self.spool_path(&intent),
                size: intent.size,
                _lease: self.local_lease(&intent.id),
            },
        );
        Ok(())
    }
    fn deletion_for(&self, path: &str, revision: &Revision) -> Result<Intent> {
        let mut intent = self.begin_observed(path, Some(revision))?;
        intent.spool = None;
        match revision {
            Revision::Local { id, .. } => {
                intent.depends_on = Some(id.clone());
                intent.parents.clear();
            }
            Revision::Cloud { id, .. } => {
                intent.parents = vec![id.clone()];
                intent.event_path = self
                    .state
                    .lock()
                    .unwrap()
                    .events
                    .get(id)
                    .context("missing selected revision")?
                    .path
                    .clone();
            }
        }
        Ok(intent)
    }
    pub(crate) fn delete(&self, path: &str) -> Result<()> {
        let revision = self
            .view()?
            .get(path)
            .cloned()
            .context("delete source missing")?;
        let intent = self.deletion_for(path, &revision)?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let mut next = s.clone();
        next.pending.push(intent);
        next.save(&self.root)?;
        *s = next;
        self.pins.lock().unwrap().remove(path);
        Ok(())
    }
    pub(crate) fn import(&self, source: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(&self.rclone, source)?;
        crate::manifest::validate_manifest(&manifest)?;
        // Import namespace without reading data. A whole-file hash is not in the
        // archive format, so use its authenticated content-root identity here.
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        let event = Event {
            version: 1,
            device: s.device.clone(),
            worker: s.worker.clone(),
            path: manifest.original_name.clone(),
            parents: vec![],
            content: Some(Content {
                hash: manifest.content_root_blake3.clone(),
                size: manifest.original_size,
                manifest,
            }),
        };
        event.validate()?;
        let id = event.id()?;
        let mut next = s.clone();
        next.events.insert(id, event);
        next.save(&self.root)?;
        *s = next;
        Ok(())
    }
    /// MOVE expresses an explicit namespace operation, unlike an unconditioned PUT.
    /// A missing destination must descend from its current deletion, not an old editor base.
    fn move_destination(&self, path: &str) -> Result<Intent> {
        let visible = self.view()?.get(path).cloned();
        let mut intent = self.begin_observed(path, visible.as_ref())?;
        let state = self.state.lock().unwrap();
        if let Some(previous) = state.pending.iter().rev().find(|i| i.path == path) {
            intent.depends_on = Some(previous.id.clone());
            intent.event_path = previous.event_path.clone();
            intent.parents.clear();
        } else if let Some(Revision::Cloud { id, .. }) = visible {
            intent.parents = vec![id.clone()];
            intent.event_path = state
                .events
                .get(&id)
                .context("move destination revision missing")?
                .path
                .clone();
        } else {
            let referenced: BTreeSet<_> = state
                .events
                .values()
                .flat_map(|event| event.parents.iter())
                .collect();
            intent.parents = state
                .events
                .iter()
                .filter(|(id, event)| event.path == path && !referenced.contains(id))
                .map(|(id, _)| id.clone())
                .collect();
            intent.event_path = path.into();
        }
        Ok(intent)
    }
    pub(crate) fn rename_file(&self, from: &str, to: &str) -> Result<()> {
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        let revision = self
            .view()?
            .get(from)
            .cloned()
            .context("rename source missing")?;
        let mut destination = self.move_destination(to)?;
        let deletion = self.deletion_for(from, &revision)?;
        match revision {
            Revision::Cloud { content, .. } => {
                let mut s = self.state.lock().unwrap();
                let mut next = s.clone();
                if destination.depends_on.is_some() {
                    // Preserve cloud bytes without hydration while waiting on earlier local intents.
                    // An explicit synchronous commit of the already committed dependency is safe.
                    let dep = destination.depends_on.as_ref().unwrap();
                    if !next.committed_intents.contains_key(dep) {
                        bail!("destination has pending local work; sync before moving this cloud revision");
                    }
                }
                let id = next.commit(&destination, Some(content.clone()))?;
                next.commit(&deletion, None)?;
                next.save(&self.root)?;
                *s = next;
                let mut pins = self.pins.lock().unwrap();
                pins.remove(from);
                pins.insert(to.into(), Revision::Cloud { id, content });
            }
            Revision::Local { path, size, .. } => {
                let output = self.spool_path(&destination);
                self.copy_to_spool(&path, &output)?;
                File::open(&output)?.sync_all()?;
                destination.size = size;
                destination.hash = crate::utils::hash_file_range(&output, 0, size)?;
                destination = self.prepare_seal(destination)?;
                let mut s = self.state.lock().unwrap();
                let mut next = s.clone();
                next.pending.push(destination.clone());
                next.pending.push(deletion);
                next.save(&self.root)?;
                *s = next;
                let mut pins = self.pins.lock().unwrap();
                pins.remove(from);
                pins.insert(
                    to.into(),
                    Revision::Local {
                        id: destination.id.clone(),
                        path: self.spool_path(&destination),
                        size,
                        _lease: self.local_lease(&destination.id),
                    },
                );
            }
        }
        Ok(())
    }
    pub(crate) fn rename_directory(&self, from: &str, to: &str) -> Result<()> {
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(&format!("{from}/")) || from.starts_with(&format!("{to}/")) {
            bail!("overlapping directory move");
        }
        let view = self.view()?;
        let prefix = format!("{from}/");
        if view
            .keys()
            .any(|p| p == to || p.starts_with(&format!("{to}/")))
        {
            bail!("directory move destination exists");
        }
        let mut changes = vec![];
        for (name, revision) in view.iter().filter(|(name, _)| name.starts_with(&prefix)) {
            let target = format!("{to}/{}", &name[prefix.len()..]);
            let mut destination = self.move_destination(&target)?;
            let deletion = self.deletion_for(name, revision)?;
            let content = match revision {
                Revision::Cloud { content, .. } => Some(content.clone()),
                Revision::Local { path, .. } => {
                    self.copy_to_spool(path, &self.spool_path(&destination))?;
                    destination = self.prepare_seal(destination)?;
                    None
                }
            };
            changes.push((name.clone(), destination, deletion, content));
        }
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        let directories: Vec<_> = next
            .directories
            .iter()
            .filter(|p| p.as_str() == from || p.starts_with(&prefix))
            .cloned()
            .collect();
        for directory in directories {
            next.directories.remove(&directory);
            next.directories
                .insert(format!("{to}{}", &directory[from.len()..]));
        }
        let mut new_pins = vec![];
        for (name, destination, deletion, content) in changes {
            let revision = if let Some(content) = content {
                let id = next.commit(&destination, Some(content.clone()))?;
                // Commit deletion only if its dependency is already committed.
                if deletion.depends_on.is_some() {
                    next.pending.push(deletion);
                } else {
                    next.commit(&deletion, None)?;
                }
                Revision::Cloud { id, content }
            } else {
                next.pending.push(destination.clone());
                next.pending.push(deletion);
                Revision::Local {
                    id: destination.id.clone(),
                    path: self.spool_path(&destination),
                    size: destination.size,
                    _lease: self.local_lease(&destination.id),
                }
            };
            new_pins.push((name, destination.path, revision));
        }
        next.save(&self.root)?;
        *state = next;
        let mut pins = self.pins.lock().unwrap();
        for (old, new, revision) in new_pins {
            pins.remove(&old);
            pins.insert(new, revision);
        }
        Ok(())
    }
    /// Metadata refresh never uploads dirty spool.
    pub(crate) fn pull(&self) -> Result<()> {
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
    pub(crate) fn sync(&self) -> Result<()> {
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
                let (manifest, manifest_remotes) = super::workspace::upload_eligible_tracked(
                    &self.rclone,
                    &self.policy,
                    &self.pool,
                    &staged,
                    &format!("virtual-{}", intent.id),
                )?;
                self.record_owned_archive(&intent, &manifest, &manifest_remotes)?;
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
        if let Some(root) = &self.shared_root {
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
    pub(crate) fn refresh_capacity(&self) -> Result<CapacityStatus> {
        let mut status = CapacityStatus::inspect(
            &crate::storage::admin::RcloneAdmin::inherited(&self.rclone),
            &self.policy,
        )?;
        let state = self.state.lock().unwrap();
        status.committed_logical_used = Some(state.logical_used()?);
        status.logical_used = state.visible_logical_used()?;
        status.usage_scope = "shared-namespace".into();
        status.spool_bytes = self.spool_bytes()?;
        status.spool_limit_bytes = self.spool_limit;
        status.pending_writes = state.pending.len();
        drop(state);
        status.logical_ceiling_estimate = status
            .logical_used
            .saturating_add(status.additional_estimate);
        if status.spool_bytes >= status.spool_limit_bytes {
            status.note.push_str(" Local spool budget reached: new growth is rejected; sync or recover retained writes.");
        }
        status.note.push_str(" Virtual mode usage is the known shared namespace plus local pending writes, not the local cache. Other unimported archives are not counted.");
        *self.capacity.lock().unwrap() = Some(status.clone());
        Ok(status)
    }
}

pub(crate) fn run(rclone: &str, args: crate::cli::MountArgs) -> Result<()> {
    if args.recover_spool {
        let paths = recover_spool(&args.workspace)?;
        println!("Exported {} local writes under recovered-writes; partial uploads are explicitly labelled. Checkpoints, spool and remote history unchanged.", paths.len());
        return Ok(());
    }
    if args.migrate_excluded {
        bail!("Active archive migration currently uses replica workspaces; virtual history is retained");
    }
    if args.stop_file.as_ref().is_some_and(|p| p.exists()) {
        bail!("stop file already exists");
    }
    let worker = args.worker_name.as_deref().unwrap_or("local");
    let mut drive = VirtualDrive::open(
        rclone,
        &args.pool,
        &args.workspace,
        worker,
        args.shared_root.as_deref(),
        args.cache_gib
            .checked_mul(1073741824)
            .context("cache limit overflow")?,
    )?;
    drive.spool_limit = args
        .spool_gib
        .checked_mul(1073741824)
        .context("spool limit overflow")?;
    let drive = Arc::new(drive);
    if args.apply_retention {
        let report = drive.apply_retention(args.keep_previous, args.exclusive_archive_ownership)?;
        println!("Retention completed: {} obsolete tracked archives, {} exact objects removed ({} logical data bytes; backend trash/versioning may delay quota recovery).", report.obsolete_archives, report.objects.len(), report.reclaimable_bytes);
        return Ok(());
    }
    if drive.root.join("retention-journal.json").exists() {
        bail!("Interrupted retention: resume --apply-retention --exclusive-archive-ownership before mounting or syncing. Do not remove the journal.");
    }
    if args.retention_report {
        println!(
            "{}",
            serde_json::to_string_pretty(&drive.retention_report(args.keep_previous)?)?
        );
        return Ok(());
    }
    for source in &args.manifests {
        drive.import(source)?;
    }
    if args.cleanup_cache {
        println!(
            "clean_cache_bytes_removed={} committed_spool_bytes_removed={}; dirty/unknown spool and remote history retained",
            drive.cache.cleanup()?, drive.cleanup_committed_spool()?
        );
        return Ok(());
    }
    if args.capacity_only {
        drive.pull()?;
    } else if args.sync_only {
        drive.sync()?;
    } else if let Err(e) = drive.sync() {
        eprintln!("Virtual sync pending, durable local state retained: {e:#}");
    }
    let report = || match drive.refresh_capacity() {
        Ok(status) => {
            println!(
                "Virtual namespace: logical_used={} additional_estimate={} pending={}",
                status.logical_used,
                status.additional_estimate,
                drive.state.lock().unwrap().pending.len()
            );
            if let Some(path) = &args.status_file {
                if let Err(e) = durable_json(path, &status) {
                    eprintln!("Status snapshot failed: {e:#}");
                }
            }
        }
        Err(e) => {
            if let Some(path) = &args.status_file {
                let _ = fs::remove_file(path);
            }
            eprintln!("Quota status unavailable: {e:#}");
        }
    };
    report();
    if args.capacity_only || args.sync_only {
        return Ok(());
    }
    let server = super::dav::Server::start(drive.clone())?;
    let mut mount = super::adapter::MountProcess::start(super::adapter::MountConfig {
        rclone: rclone.into(),
        files_dir: drive.root.join("anchor"),
        cache_dir: drive.root.join("vfs-cache"),
        target: args.mountpoint.context("mountpoint required")?,
        shared: true,
        webdav: Some((format!("http://{}/", server.address), server.token.clone())),
    })?;
    println!("Virtual drive starting: metadata-only listing, verified shard reads, durable local write spool. Incoming updates to served paths appear as incoming revision copies until remount; no distributed locking guarantee.");
    let start = std::time::Instant::now();
    let mut ready = false;
    let mut last = std::time::Instant::now();
    let mut job: Option<std::thread::JoinHandle<()>> = None;
    loop {
        for line in mount.logs() {
            eprintln!("{line}");
        }
        if let Some(code) = mount.poll()? {
            bail!("virtual mount exited {code}; spool and cache retained");
        }
        if args.stop_file.as_ref().is_some_and(|p| p.exists()) {
            break;
        }
        if !ready && mount.ready() {
            ready = true;
            println!("Virtual filesystem ready. Save acknowledges local spool, not completed cloud replication.");
        }
        if !ready && start.elapsed() > std::time::Duration::from_secs(30) {
            bail!("virtual mount readiness timeout; state retained");
        }
        if job.as_ref().is_some_and(|j| j.is_finished()) {
            let _ = job.take().unwrap().join();
            report();
        }
        if ready
            && job.is_none()
            && last.elapsed() >= std::time::Duration::from_secs(args.interval_seconds)
        {
            let drive = drive.clone();
            job = Some(std::thread::spawn(move || {
                if let Err(e) = drive.sync() {
                    eprintln!("Virtual sync pending: {e:#}");
                }
            }));
            last = std::time::Instant::now();
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let stopped = mount.stop()?;
    if let Some(job) = job {
        let _ = job.join();
    }
    drive.sync()?;
    report();
    println!(
        "Virtual mount stopped; forced={} pending spool/cache/history retained; verified committed spool may be reclaimed",
        stopped.forced
    );
    drop(server);
    Ok(())
}

fn checked_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("managed directory must not be a symlink");
    }
    Ok(())
}
pub(crate) fn recover_spool(root: &Path) -> Result<Vec<PathBuf>> {
    checked_directory(root)?;
    super::adapter::preflight_virtual(root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("virtual.lock"))?;
    lock.try_lock()
        .context("stop the virtual drive before recovery")?;
    checked_directory(&root.join("spool"))?;
    let output = root.join("recovered-writes");
    fs::create_dir_all(&output)?;
    checked_directory(&output)?;
    let mut paths = vec![];
    for entry in fs::read_dir(root.join("spool"))? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid spool identity");
        }
        checked_directory(&entry.path())?;
        let source = entry.path().join("content");
        if !source.exists() {
            continue;
        }
        let meta = fs::symlink_metadata(&source)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("invalid spool file");
        }
        let receipt: Option<Intent> =
            crate::utils::read_json(&entry.path().join("intent.json")).ok();
        let verified = receipt.as_ref().is_some_and(|i| {
            i.id == id
                && i.spool.as_deref() == Some(&id)
                && i.size == meta.len()
                && crate::utils::hash_file_range(&source, 0, meta.len())
                    .is_ok_and(|hash| hash == i.hash)
        });
        let target = output.join(format!(
            "{id}.{}.bin",
            if verified { "sealed" } else { "partial" }
        ));
        let mut temp = tempfile::NamedTempFile::new_in(&output)?;
        std::io::copy(&mut File::open(&source)?, &mut temp)?;
        temp.as_file().sync_all()?;
        if target.exists() {
            if crate::utils::hash_file_range(&target, 0, fs::metadata(&target)?.len())?
                != crate::utils::hash_file_range(temp.path(), 0, temp.as_file().metadata()?.len())?
            {
                bail!("recovery output already exists with different bytes");
            }
        } else {
            temp.persist_noclobber(&target).map_err(|e| e.error)?;
        }
        if let Some(receipt) = receipt {
            durable_json(&output.join(format!("{id}.json")), &receipt)?;
        }
        paths.push(target);
    }
    #[cfg(unix)]
    File::open(output)?.sync_all()?;
    Ok(paths)
}

#[cfg(test)]
pub(crate) fn fixture(root: &Path) -> VirtualDrive {
    for path in ["spool", "clean-cache", ".rpool"] {
        fs::create_dir_all(root.join(path)).unwrap();
    }
    let mut state = Namespace::create("tester").unwrap();
    state.save(root).unwrap();
    VirtualDrive {
        root: root.into(),
        state: Mutex::new(state),
        policy: PoolDefinition::default(),
        pool: "test".into(),
        rclone: "nonexistent-rclone".into(),
        shared_root: None,
        cache: ShardCache::new(root.join("clean-cache"), 1024).unwrap(),
        capacity: Mutex::new(None),
        sync_gate: Mutex::new(()),
        pins: Mutex::new(BTreeMap::new()),
        local_leases: Mutex::new(BTreeMap::new()),
        spool_limit: 64 * 1073741824,
        spool_writes: Mutex::new(()),
        _lock: File::create(root.join("virtual.lock")).unwrap(),
    }
}
