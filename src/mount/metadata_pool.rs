//! Checkpoints on real pools: replica directories over `SharedTransport`,
//! the newest metadata generation of a saved pool (CLI, doctor), and the
//! mounted drive's automatic compaction.
use super::metadata_cache::Cache;
use super::metadata_checkpoint::Replica;
use super::metadata_checkpoint_model::Family;
use super::metadata_compaction::{compact, enable_gate, Config, Options, Report};
use super::shared_transport::SharedTransport;
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

/// Owned directories of one replica.
pub(crate) struct ReplicaDirs {
    records: Vec<SharedTransport>,
    heads: SharedTransport,
    chunks: SharedTransport,
    marks: SharedTransport,
}
impl ReplicaDirs {
    pub(crate) fn replica(&self) -> Replica<'_> {
        Replica {
            records: self.records.iter().map(|t| t as _).collect(),
            heads: &self.heads,
            chunks: &self.chunks,
            marks: &self.marks,
        }
    }
}

/// `roots` are family roots (v6 `events-v6/…`).
pub(crate) fn replica_dirs(
    rclone: &str,
    roots: &[String],
    native_crypt: bool,
    family: &Family,
) -> Result<Vec<ReplicaDirs>> {
    let open =
        |root: &str| SharedTransport::new(rclone, root).map(|t| t.with_native_crypt(native_crypt));
    roots
        .iter()
        .map(|root| {
            let records = family
                .kinds
                .iter()
                .map(|kind| match family.name {
                    // v6 events live in `<root>/events`; other families keep
                    // each kind in `<root>/<kind>/events`.
                    "v6" => open(root),
                    _ => open(&crate::utils::remote_join(root, kind)),
                })
                .collect::<Result<_>>()?;
            let checkpoints = crate::utils::remote_join(root, "checkpoints");
            Ok(ReplicaDirs {
                records,
                heads: open(&checkpoints)?.in_dir("heads"),
                chunks: open(&checkpoints)?.in_dir("chunks"),
                marks: open(&checkpoints)?.in_dir("marks"),
            })
        })
        .collect()
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The metadata a saved pool uses now: its newest generation.
pub(crate) struct Target {
    pub family: Family,
    pub roots: Vec<String>,
    pub native_crypt: bool,
    pub epoch: Option<String>,
}
pub(crate) fn pool_target(rclone: &str, pool: &str) -> Result<Option<Target>> {
    let policy = crate::pool::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .with_context(|| format!("pool not found: {pool}"))?;
    crate::pool::validate_pool(&policy)?;
    let generations = crate::pool::browse_generations::discover(rclone, pool, &policy.remotes)?;
    let Some(newest) = generations.first() else {
        return Ok(None);
    };
    let roots = super::pool_sync::roots(pool, &policy.remotes)?
        .into_iter()
        .map(|root| match &newest.epoch {
            Some(epoch) => crate::utils::remote_join(&root, &format!("epochs/{epoch}")),
            None => root,
        })
        .collect();
    Ok(Some(Target {
        family: Family::v6(),
        roots,
        native_crypt: policy.native_crypt,
        epoch: newest.epoch.clone(),
    }))
}

/// `rpool pool compact`: reads every checkpoint (no workspace cache).
pub(crate) fn compact_pool(
    rclone: &str,
    pool: &str,
    dry_run: bool,
    enable_deletion: bool,
) -> Result<Option<Report>> {
    let Some(target) = pool_target(rclone, pool)? else {
        return Ok(None);
    };
    let dirs = replica_dirs(rclone, &target.roots, target.native_crypt, &target.family)?;
    let replicas: Vec<_> = dirs.iter().map(ReplicaDirs::replica).collect();
    if enable_deletion && !dry_run {
        enable_gate(&target.family, &replicas)?;
    }
    let options = Options {
        now: now_unix(),
        dry_run,
        force: true,
        config: Config::load()?,
    };
    let mut report = compact(&target.family, &replicas, &mut Cache::default(), &options)?;
    if enable_deletion && dry_run {
        report
            .notes
            .push("--enable-deletion is not applied on a dry run".into());
    }
    Ok(Some(report))
}

impl VirtualDrive {
    /// One automatic pass for a pool-sync drive; `None` when the
    /// drive has no pool-sync metadata or automatic compaction is off.
    pub(crate) fn compact_metadata(&self) -> Result<Option<Report>> {
        if self.pool_sync_roots.is_empty() {
            return Ok(None);
        }
        let config = Config::load()?;
        if !config.auto {
            return Ok(None);
        }
        let family = Family::v6();
        let dirs = replica_dirs(
            &self.rclone,
            &self.pool_sync_roots,
            self.policy.native_crypt,
            &family,
        )?;
        let replicas: Vec<_> = dirs.iter().map(ReplicaDirs::replica).collect();
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync gate poisoned"))?;
        let path = Cache::path(&self.root, family.name);
        let mut cache = Cache::load(&path);
        let options = Options {
            now: now_unix(),
            dry_run: false,
            force: false,
            config,
        };
        let report = compact(&family, &replicas, &mut cache, &options)?;
        cache.save(&path)?;
        Ok(Some(report))
    }
}

/// Monitoring alert for a compaction pass (`None`: nothing to warn about).
pub(crate) fn growth_alert(
    pool: &str,
    result: &Result<Option<Report>>,
    now: u64,
) -> Option<crate::monitor::model::Alert> {
    use super::metadata_limits::{WARN_BYTES, WARN_RECORDS};
    let message = match result {
        Err(error) => format!("metadata compaction failed: {error:#}"),
        Ok(Some(report))
            if report.uncovered >= WARN_RECORDS || report.uncovered_bytes >= WARN_BYTES =>
        {
            format!(
                "{} metadata records are not in a checkpoint; run `rpool pool compact {pool}`",
                report.uncovered
            )
        }
        Ok(_) => return None,
    };
    Some(crate::monitor::model::Alert {
        kind: crate::monitor::model::AlertKind::MetadataGrowing,
        remote: None,
        since_unix: now,
        message,
    })
}

/// v6 events of a saved pool generation for `rpool pool browse`, including
/// checkpointed ones (read-only).
pub(crate) fn read_v6(
    rclone: &str,
    pool: &str,
    policy: &PoolDefinition,
    epoch: Option<&str>,
) -> Result<BTreeMap<String, super::shared_model::Event>> {
    use super::pool_sync::{list_unseen, read_unseen};
    let stores = super::pool_sync::read_stores(rclone, pool, policy, epoch)?;
    let stores: Vec<_> = stores.iter().map(|s| s.as_ref()).collect();
    let roots: Vec<_> = super::pool_sync::roots(pool, &policy.remotes)?
        .into_iter()
        .map(|root| match epoch {
            Some(epoch) => crate::utils::remote_join(&root, &format!("epochs/{epoch}")),
            None => root,
        })
        .collect();
    let family = Family::v6();
    let dirs = replica_dirs(rclone, &roots, policy.native_crypt, &family)?;
    let replicas: Vec<_> = dirs.iter().map(ReplicaDirs::replica).collect();
    let known = BTreeSet::new();
    let unseen = list_unseen(&stores, &known)?;
    let mut events = BTreeMap::new();
    super::metadata_checkpoint::pull(
        &family,
        &replicas,
        &mut Cache::default(),
        unseen.gate,
        true,
        &mut |_, id, text| {
            let event: super::shared_model::Event = serde_json::from_str(text)?;
            event.validate()?;
            if event.id()? != id {
                bail!("checkpoint event identity mismatch");
            }
            events.insert(id.to_owned(), event);
            Ok(())
        },
    )?;
    let tail = read_unseen(&stores, &unseen, |id| events.contains_key(id))?;
    events.extend(tail);
    Ok(events)
}

/// Cheap metadata counts of a saved pool for `rpool doctor`: listings and
/// heads only, no chunk reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Stats {
    pub family: String,
    pub epoch: Option<String>,
    pub records: usize,
    pub record_bytes: u64,
    pub checkpoints: usize,
    pub newest_checkpoint_unix: Option<u64>,
    /// Records the newest checkpoint covers (may include deleted ones).
    pub checkpointed: u64,
    pub deletion_enabled: bool,
}
pub(crate) fn pool_stats(rclone: &str, pool: &str) -> Result<Option<Stats>> {
    use super::metadata_checkpoint::{list_all, read_any};
    let Some(target) = pool_target(rclone, pool)? else {
        return Ok(None);
    };
    let dirs = replica_dirs(rclone, &target.roots, target.native_crypt, &target.family)?;
    let replicas: Vec<_> = dirs.iter().map(ReplicaDirs::replica).collect();
    let mut stats = Stats {
        family: target.family.name.into(),
        epoch: target.epoch.clone(),
        ..Default::default()
    };
    let gate = target.family.gate_id();
    for kind in 0..target.family.kinds.len() {
        let records: Vec<_> = replicas.iter().map(|r| r.records[kind]).collect();
        let mut listing = list_all(&records)?;
        if kind == 0 {
            stats.deletion_enabled = listing.remove(&gate).is_some();
        }
        stats.records += listing.len();
        stats.record_bytes += listing.values().map(|(size, _)| size).sum::<u64>();
    }
    let heads: Vec<_> = replicas.iter().map(|r| r.heads).collect();
    let listing = list_all(&heads)?;
    for id in listing.keys() {
        let _ = read_any(&heads, &listing, id, |bytes| {
            let head = super::metadata_checkpoint_model::Head::parse(id, bytes, &target.family)?;
            stats.checkpoints += 1;
            if stats
                .newest_checkpoint_unix
                .is_none_or(|t| head.created_unix >= t)
            {
                stats.newest_checkpoint_unix = Some(head.created_unix);
                stats.checkpointed = head.records;
            }
            Ok(())
        });
    }
    Ok(Some(stats))
}
