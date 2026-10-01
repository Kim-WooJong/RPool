//! Opening a virtual drive workspace (normal, epoch-pinned and recovery-only).

use super::*;

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
            shared: Option<String>,
            policy: PoolDefinition,
            metadata_roots: Vec<String>,
        }
        let binding: Binding = crate::utils::read_json(&root.join("virtual.json"))?;
        if !matches!(binding.version, 6 | 7) || binding.metadata_roots.is_empty() {
            bail!("account recovery requires an existing v6/v7 pool workspace");
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
        let history_limit = if binding.version == 7 {
            let value: crate::mount::pool_sync::Config =
                crate::utils::read_json(&root.join("pool-sync-config.json"))?;
            value.history_limit
        } else {
            0
        };
        Ok(Self {
            root: root.into(),
            state: Mutex::new(state),
            policy: binding.policy,
            pool: binding.pool,
            rclone: rclone.into(),
            shared_root: binding.shared,
            cache,
            capacity: Mutex::new(None),
            sync_gate: Mutex::new(()),
            pins: Mutex::new(BTreeMap::new()),
            local_leases: Mutex::new(BTreeMap::new()),
            spool_limit: 0,
            spool_writes: Mutex::new(()),
            bounded_shared: false,
            peer_retention: binding.version == 7,
            pool_sync_roots: binding.metadata_roots,
            pool_history_limit: history_limit,
            peer_read_pins: Mutex::new(BTreeMap::new()),
            checkpoint_coordinator: false,
            checkpoint_keep: 0,
            layout_deferral: None,
            history_retention: None,
            _lock: lock,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        shared: Option<&str>,
        cache_limit: u64,
        bounded_shared: bool,
        pool_sync: bool,
        peer_retention: bool,
    ) -> Result<Self> {
        Self::open_internal(
            rclone,
            pool,
            root,
            worker,
            shared,
            cache_limit,
            bounded_shared,
            pool_sync,
            peer_retention,
            None,
        )
    }

    /// Initialize a private transition generation; callers persist the epoch in their journal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_with_epoch(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        shared: Option<&str>,
        cache_limit: u64,
        bounded_shared: bool,
        pool_sync: bool,
        peer_retention: bool,
        epoch: &str,
    ) -> Result<Self> {
        if root.join("virtual.json").exists() {
            bail!("epoch initialization requires a fresh workspace");
        }
        Self::open_internal(
            rclone,
            pool,
            root,
            worker,
            shared,
            cache_limit,
            bounded_shared,
            pool_sync,
            peer_retention,
            Some(epoch),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_internal(
        rclone: &str,
        pool: &str,
        root: &Path,
        worker: &str,
        shared: Option<&str>,
        cache_limit: u64,
        bounded_shared: bool,
        pool_sync: bool,
        peer_retention: bool,
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
            if !pool_sync {
                bail!("workspace epoch requires pool sync");
            }
        }
        if pool_sync {
            Event {
                version: 1,
                worker: worker.into(),
                device: "validation".into(),
                path: "validation".into(),
                parents: vec![],
                content: None,
            }
            .validate()?;
        }
        let auto_roots = if pool_sync {
            let policy = crate::pool::load_pool_store()?
                .pools
                .get(pool)
                .cloned()
                .context("unknown pool")?;
            crate::pool::validate_pool(&policy)?;
            drive_metadata_roots(pool, &policy.remotes, epoch.as_deref(), peer_retention)?
        } else {
            vec![]
        };
        let shared_name = if pool_sync {
            auto_roots.first().cloned()
        } else {
            shared.map(|root| {
                format!(
                    "{}/{}",
                    root.trim_end_matches('/'),
                    if bounded_shared {
                        "virtual-v5"
                    } else {
                        "virtual-v3"
                    }
                )
            })
        };
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
        let current_policy = crate::pool::load_pool_store()?
            .pools
            .get(pool)
            .cloned()
            .context("unknown pool")?;
        crate::pool::validate_pool(&current_policy)?;
        let mut binding: Binding = if config.exists() {
            crate::utils::read_json(&config)?
        } else {
            let policy = crate::pool::load_pool_store()?
                .pools
                .get(pool)
                .cloned()
                .context("unknown pool")?;
            let b = Binding {
                version: if peer_retention {
                    7
                } else if pool_sync {
                    6
                } else if bounded_shared {
                    5
                } else {
                    1
                },
                pool: pool.into(),
                shared: shared.map(str::to_owned),
                metadata_roots: auto_roots.clone(),
                epoch: epoch.clone(),
                policy,
            };
            durable_json(&config, &b)?;
            b
        };
        if binding.version
            != (if peer_retention {
                7
            } else if pool_sync {
                6
            } else if bounded_shared {
                5
            } else {
                1
            })
            || binding.epoch != epoch
            || binding.pool != pool
            || binding.shared.as_deref() != shared
            || binding.metadata_roots != auto_roots
        {
            bail!("workspace pool/shared-root changed; keep this workspace and use Apply pool changes to transition its account membership. Reprocess alone does not update mount metadata");
        }
        crate::mount::layout_refresh::validate_membership(&binding.policy, &current_policy)?;
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
        let mut state = Namespace::load(&root, worker)?;
        if bounded_shared || pool_sync {
            state.version = if peer_retention {
                7
            } else if pool_sync {
                6
            } else {
                5
            };
            state.save(&root)?;
        }
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
            shared_root: shared.map(str::to_owned),
            cache,
            capacity: Mutex::new(None),
            sync_gate: Mutex::new(()),
            pins: Mutex::new(BTreeMap::new()),
            local_leases: Mutex::new(BTreeMap::new()),
            spool_limit: 64 * 1073741824,
            spool_writes: Mutex::new(()),
            bounded_shared,
            peer_retention,
            pool_sync_roots: auto_roots,
            pool_history_limit: 0,
            peer_read_pins: Mutex::new(BTreeMap::new()),
            checkpoint_coordinator: false,
            checkpoint_keep: 0,
            layout_deferral,
            history_retention: crate::drive_history::retention::explicit(pool),
            _lock: lock,
        })
    }
}
