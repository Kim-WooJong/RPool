//! Production side of the cleanup: the journal on the newest generation's
//! metadata replicas, the fresh observation (every generation's events
//! including checkpointed ones, record times, purge marks, this PC's open
//! drive, and the reference sources of the migration cleanup), and object
//! deletion through the pool's storage writer.
use super::execute::CleanupIo;
use super::records::{self, Record};
use super::select::{folders, is_drive_folder, World};
use crate::drive_history::marks::{self, MarkStore};
use crate::migration::retire::live_refs;
use crate::migration::retire::refs::References;
use crate::mount::history_bridge::{self as bridge, VirtualDrive};
use crate::prelude::*;
use crate::storage::reader::StorageReader;

pub(crate) struct LiveIo<'a> {
    rclone: String,
    pool: String,
    policy: PoolDefinition,
    stores: Vec<marks::Remote>,
    local: Option<&'a VirtualDrive>,
    worker: String,
    /// Seconds added to the clock (tests run "days later").
    pub(crate) clock_offset: u64,
    /// Set when the mount is shutting down.
    pub(crate) stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl<'a> LiveIo<'a> {
    pub(crate) fn open(rclone: &str, pool: &str, local: Option<&'a VirtualDrive>) -> Result<Self> {
        let policy = super::super::load::pool_definition(pool)?;
        let generation = super::super::load::generation(rclone, pool)?
            .with_context(|| format!("pool {pool} has no online drive (pool-sync metadata) yet"))?;
        let roots = super::super::load::generation_roots(pool, &generation)?;
        let stores = marks::Remote::open(rclone, &roots, policy.native_crypt)?;
        Ok(Self {
            rclone: rclone.into(),
            pool: pool.into(),
            policy,
            stores,
            local,
            worker: local.map_or_else(super::super::dispatch::cloud_worker, bridge::worker),
            clock_offset: 0,
            stop: None,
        })
    }

    fn stores(&self) -> Vec<&dyn MarkStore> {
        self.stores.iter().map(|s| s as _).collect()
    }
}

impl CleanupIo for LiveIo<'_> {
    fn records(&self) -> Result<Vec<Record>> {
        records::read(&self.stores())
    }
    fn publish(&self, record: &Record) -> Result<()> {
        records::publish(&self.stores(), record)
    }
    fn observe(&self) -> Result<World> {
        observe(self)
    }
    fn delete(&self, address: &str) -> Result<()> {
        crate::migration::retire::live::delete_object(
            &self.rclone,
            self.policy.native_crypt,
            address,
        )
    }
    fn now(&self) -> u64 {
        crate::utils::now_unix() + self.clock_offset
    }
    fn worker(&self) -> String {
        self.worker.clone()
    }
    fn stopped(&self) -> bool {
        self.stop
            .as_ref()
            .is_some_and(|s| s.load(std::sync::atomic::Ordering::Acquire))
    }
}

fn observe(io: &LiveIo<'_>) -> Result<World> {
    let now = io.now();
    let (rclone, pool, policy) = (io.rclone.as_str(), io.pool.as_str(), &io.policy);
    let mut uncertain = Vec::new();
    let pool_roots: BTreeSet<String> =
        crate::remote_root::apply_remote_roots(policy.remotes.clone())?
            .into_iter()
            .collect();
    let listings = crate::migration::retire::observe::list_roots(rclone, &pool_roots);
    let mut histories = Vec::new();
    let mut all_times = BTreeMap::new();
    let mut all_purged = BTreeSet::new();
    match crate::pool::browse_generations::discover(rclone, pool, &policy.remotes) {
        Err(error) => uncertain.push(format!("drive generations: {error:#}")),
        Ok(generations) => {
            for generation in generations {
                let epoch = generation.epoch.as_deref();
                let label = format!(
                    "drive generation {}",
                    epoch.map_or("initial", |e| &e[..e.len().min(12)])
                );
                let roots = super::super::load::generation_roots(pool, &generation)?;
                // A missing time never expires and a missing purge mark keeps
                // the entry: both only keep more.
                let (times, _) = super::super::times::list(rclone, &roots);
                let purged = marks::Remote::open(rclone, &roots, policy.native_crypt)
                    .and_then(|stores| {
                        let stores: Vec<&dyn MarkStore> = stores.iter().map(|s| s as _).collect();
                        marks::purged(&stores)
                    })
                    .unwrap_or_default();
                let built = crate::mount::metadata_pool::read_v6(rclone, pool, policy, epoch)
                    .and_then(|events| {
                        super::super::source_v6::build(
                            events,
                            &times,
                            &BTreeSet::new(),
                            now,
                            purged.clone(),
                        )
                    });
                match built {
                    Ok(history) => histories.push((label, history)),
                    Err(error) => uncertain.push(format!("{label}: {error:#}")),
                }
                all_times.extend(times);
                all_purged.extend(purged);
            }
        }
    }
    let mut local_kept = Vec::new();
    if let Some(drive) = io.local {
        let (events, unpublished) = bridge::v6_events(drive);
        match super::super::source_v6::build(events, &all_times, &unpublished, now, all_purged) {
            Ok(history) => histories.push(("this PC's drive".into(), history)),
            Err(error) => uncertain.push(format!("this PC's drive: {error:#}")),
        }
        local_kept = bridge::kept_contents(drive)
            .into_iter()
            .map(|(label, content)| (label, content.manifest))
            .collect();
    }
    let named: BTreeSet<String> = histories
        .iter()
        .flat_map(|(_, h)| h.payloads.values())
        .flat_map(|p| folders(&p.manifest))
        .collect();
    let reader = StorageReader::rclone(rclone);
    let sources = live_refs::Sources {
        rclone,
        pool,
        policy,
        migration_id: "",
        listings: &listings,
        subjects: &named,
        workspaces: &[],
        reader: &reader,
    };
    let mut refs = References::default();
    live_refs::inventory(&sources, &mut refs);
    live_refs::cloud_manifests(&sources, &mut refs);
    live_refs::other_migrations(&sources, &mut refs);
    unnamed_uploads(&sources, &mut refs);
    Ok(World {
        histories,
        local_kept,
        refs,
        listings,
        uncertain,
    })
}

/// Drive uploads no event names (yet): an upload in progress on some PC,
/// or a finished one whose event is not published. Whatever they name is
/// kept (an incremental upload reuses older archives' shards).
fn unnamed_uploads(sources: &live_refs::Sources<'_>, refs: &mut References) {
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, listing) in sources.listings {
        let Some(files) = listing.files() else {
            continue;
        };
        for (id, _) in crate::migration::enumerate::manifest_ids(files) {
            if is_drive_folder(&id) && !sources.subjects.contains(&id) {
                by_id
                    .entry(id.clone())
                    .or_default()
                    .push(crate::utils::remote_join(
                        root,
                        &format!("{id}/manifest.json"),
                    ));
            }
        }
    }
    for (id, addresses) in by_id {
        let mut errors = Vec::new();
        let mut found = None;
        for address in &addresses {
            match crate::manifest::load_manifest_with_storage(sources.reader, address) {
                Ok(manifest) => {
                    found = Some(manifest);
                    break;
                }
                Err(error) => errors.push(format!("{address}: {error:#}")),
            }
        }
        match found {
            Some(manifest) => refs.add_manifest(&format!("drive upload {id}"), &manifest),
            None => refs.uncertain(format!("drive upload {id}: {}", errors.join("; "))),
        }
    }
}
