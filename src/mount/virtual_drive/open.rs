//! Opening a virtual drive workspace (normal, epoch-pinned and recovery-only).

use super::*;

/// The only workspace and namespace format: pool sync (v6).
pub(crate) const FORMAT_VERSION: u32 = 6;

impl VirtualDrive {
    /// Recovery-only opener: never initializes, saves, synchronizes or cleans source state.
    pub(crate) fn open_recovery_source(
        rclone: &str,
        root: &Path,
        cache: ShardCache,
    ) -> Result<Self> {
        #[derive(Deserialize)]
        struct Binding {
            version: u32,
            pool: String,
            policy: PoolDefinition,
            metadata_roots: Vec<String>,
        }
        let binding: Binding = crate::utils::read_json(&root.join("virtual.json"))?;
        if binding.version != FORMAT_VERSION || binding.metadata_roots.is_empty() {
            bail!("account recovery requires an existing v6 pool workspace");
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("virtual.lock"))?;
        lock.try_lock()
            .context("stop the source mount before recovery")?;
        if !root.join("namespace.json").is_file() {
            bail!("source primary namespace missing; preserve workspace");
        }
        let state = Namespace::load(root, "recovery-reader")?;
        if state.version != binding.version {
            bail!("source binding/namespace version mismatch");
        }
        Ok(Self {
            root: root.into(),
            state: Mutex::new(state),
            policy: binding.policy,
            pool: binding.pool,
            rclone: rclone.into(),
            cache,
            capacity: Mutex::new(None),
            sync_gate: Mutex::new(()),
            pins: Mutex::new(BTreeMap::new()),
            local_leases: Mutex::new(BTreeMap::new()),
            spool_limit: 0,
            spool_writes: Mutex::new(()),
            pool_sync_roots: binding.metadata_roots,
            peer_read_pins: Mutex::new(BTreeMap::new()),
            layout_deferral: None,
            upload: UploadControl::default(),
            _lock: lock,
        })
    }

    pub(crate) fn open(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        cache_limit: u64,
    ) -> Result<Self> {
        Self::open_internal(rclone, pool, root, worker, cache_limit, None)
    }

    /// Initialize a private transition generation; callers persist the epoch in their journal.
    pub(crate) fn open_with_epoch(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        cache_limit: u64,
        epoch: &str,
    ) -> Result<Self> {
        if root.join("virtual.json").exists() {
            bail!("epoch initialization requires a fresh workspace");
        }
        Self::open_internal(rclone, pool, root, worker, cache_limit, Some(epoch))
    }

    pub(super) fn open_internal(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        cache_limit: u64,
        requested_epoch: Option<&str>,
    ) -> Result<Self> {
        #[derive(Deserialize)]
        struct Epoch {
            #[serde(default)]
            epoch: Option<String>,
        }
        let epoch = if root.join("virtual.json").exists() {
            crate::utils::read_json::<Epoch>(&root.join("virtual.json"))?.epoch
        } else {
            requested_epoch.map(str::to_owned)
        };
        if let Some(value) = &epoch {
            validate_workspace_epoch(value)?;
        }
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
        Event {
            version: 1,
            worker: worker.into(),
            device: "validation".into(),
            path: "validation".into(),
            parents: vec![],
            content: None,
        }
        .validate()?;
        let current_policy = crate::pool::load_pool_store()?
            .pools
            .get(pool)
            .cloned()
            .context("unknown pool")?;
        crate::pool::validate_pool(&current_policy)?;
        let auto_roots = drive_metadata_roots(pool, &current_policy.remotes, epoch.as_deref())?;
        let shared = auto_roots.first().cloned();
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
        if requested_epoch.is_some() && existing {
            bail!("epoch initialization requires a fresh workspace");
        }
        #[derive(Serialize, Deserialize)]
        struct Binding {
            version: u32,
            pool: String,
            shared: Option<String>,
            policy: PoolDefinition,
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            metadata_roots: Vec<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            epoch: Option<String>,
        }
        let mut binding: Binding = if existing {
            crate::utils::read_json(&config)?
        } else {
            let b = Binding {
                version: FORMAT_VERSION,
                pool: pool.into(),
                shared: shared.clone(),
                metadata_roots: auto_roots.clone(),
                epoch: epoch.clone(),
                policy: current_policy.clone(),
            };
            durable_json(&config, &b)?;
            b
        };
        if binding.version != FORMAT_VERSION {
            bail!(
                "this workspace uses a removed drive mode (format v{}); create a new workspace",
                binding.version
            );
        }
        if binding.epoch != epoch
            || binding.pool != pool
            || binding.shared != shared
            || binding.metadata_roots != auto_roots
        {
            bail!("workspace pool changed; keep this workspace and use Apply pool changes to transition its account membership. Reprocess alone does not update mount metadata");
        }
        crate::mount::layout_refresh::validate_membership(&binding.policy, &current_policy)?;
        for name in ["spool", "clean-cache", "anchor", ".rpool"] {
            fs::create_dir_all(root.join(name))?;
            checked_directory(&root.join(name))?;
        }
        if let Some(root) = &shared {
            SharedTransport::new(rclone, root)?;
        }
        if existing && !root.join("namespace.json").exists() {
            bail!("existing virtual workspace lost its primary namespace; preserve spool and use recovery");
        }
        let mut state = Namespace::load(&root, worker)?;
        state.version = FORMAT_VERSION;
        state.save(&root)?;
        let cache = ShardCache::new(root.join("clean-cache"), cache_limit)?;
        // Started uploads resume with the layout they were planned with; a
        // layout change waits for them (see `layout_refresh`).
        let pending = crate::mount::layout_refresh::pending_layout_work(
            &root,
            state.pending.iter().map(|intent| intent.id.as_str()),
        )?;
        let (effective_policy, layout_deferral) =
            crate::mount::layout_refresh::resolve(&binding.policy, &current_policy, pending)?;
        if serde_json::to_value(&binding.policy)? != serde_json::to_value(&effective_policy)? {
            binding.policy = effective_policy;
            durable_json(&config, &binding)?;
        }
        Ok(Self {
            root,
            state: Mutex::new(state),
            policy: binding.policy,
            pool: pool.into(),
            rclone: rclone.into(),
            cache,
            capacity: Mutex::new(None),
            sync_gate: Mutex::new(()),
            pins: Mutex::new(BTreeMap::new()),
            local_leases: Mutex::new(BTreeMap::new()),
            spool_limit: 64 * 1073741824,
            spool_writes: Mutex::new(()),
            pool_sync_roots: auto_roots,
            peer_read_pins: Mutex::new(BTreeMap::new()),
            layout_deferral,
            upload: UploadControl::default(),
            _lock: lock,
        })
    }
}
