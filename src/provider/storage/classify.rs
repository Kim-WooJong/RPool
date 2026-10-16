//! Which group every unit on the provider belongs to (pure; fresh inputs per
//! run, built by `observe`).
//!
//! Archive folders are classified by what names them, never by their name.
//! "In use" errs towards keeping: any mention counts (`References::
//! referrers`: the folder name or any of its objects, anywhere in the
//! events). Owning, which makes data deletable, is structured: only the
//! content manifests of a generation's events (`Generation::owns`).
//!
//! 1. named by a generation in use (current, newer than the current one, or
//!    of a migration in progress), by a manifest, the inventory, a drive
//!    upload no metadata names yet, or a migration in progress: **in use**;
//! 2. else named by a finished or abandoned migration: **migration
//!    leftovers** (kept; `pool migrate retire` owns them);
//! 3. else a `put` archive (not a drive archive) with a manifest: an
//!    **unknown put archive**, kept (drive metadata does not own uploads);
//! 4. else owned by older generations or foreign pools only: one group per
//!    set of such owners, deletable when every owner is (superseded
//!    generations freely, unconfirmed ones and foreign pools only with an
//!    explicit acknowledgement);
//! 5. else: an **incomplete upload** (drive archive, or no manifest
//!    anywhere), or an **unknown put archive**, both kept.
//!
//! Metadata generations follow their status; their deletion waits for every
//! archive on this provider that only they name (`Placed::depends`). Copies
//! here of a configured pool that does not use this provider take their
//! status from that pool's own metadata ([`copy_status`]); its migration
//! journals are kept. Speed
//! test files older than a day may go, marked like everything else but with
//! a grace period of [`SPEED_TEST_GRACE`]. Everything else is kept. While any source is uncertain nothing is deletable.
use super::layout::{self, Listing, Unit, UnitKind};
use super::model::{Ack, Group, GroupKind, Pending};
use crate::migration::retire::guard::Totals;
use crate::migration::retire::model::RetireObject;
use crate::migration::retire::refs::References;
use crate::prelude::*;

/// Speed-test files younger than this may belong to a test still running.
pub(crate) const SPEED_TEST_AGE: u64 = 24 * 60 * 60;
/// Grace period of speed-test leftovers between marking and deleting.
pub(crate) const SPEED_TEST_GRACE: u64 = 60 * 60;
/// An older generation no migration replaced that was written this recently
/// is treated as in use (14 days): a PC may still work on it.
pub(crate) const RECENT_GENERATION: u64 = 14 * 24 * 60 * 60;

/// Status of one drive metadata generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenerationStatus {
    /// The generation readers of the pool use now.
    Current,
    /// Not the current one, but written after it or within the last
    /// [`RECENT_GENERATION`] seconds: some PC probably still uses it.
    Newer,
    /// Replaced by a recorded migration adoption.
    Superseded {
        /// Migration whose adoption replaced it.
        migration: String,
    },
    /// Older, and no recorded migration replaced it (a pool change applied
    /// on some PC): a PC whose drive was not switched may still use it.
    Unconfirmed,
    /// Not adopted (yet) by the migration that wrote it, which is in progress.
    MigrationInProgress {
        /// The migration.
        migration: String,
    },
    /// Not adopted by the migration that wrote it, which is over.
    MigrationLeftover {
        /// The migration.
        migration: String,
    },
    /// Of a pool not configured on this PC (read from this provider only).
    /// A copy here of a configured pool that does not use this provider
    /// gets a status of its own pool instead ([`copy_status`]).
    Foreign,
}

/// One drive metadata generation and everything its events name.
#[derive(Debug, Clone)]
pub(crate) struct Generation {
    /// Configured pool, if any.
    pub pool: Option<String>,
    /// Pool-sync scope (64 hex).
    pub scope: String,
    /// Epoch; `None` for the first generation.
    pub epoch: Option<String>,
    /// What it is.
    pub status: GenerationStatus,
    /// Every string of its events (including checkpointed ones): decides
    /// what is in use.
    pub refs: References,
    /// Archive folders its events' content manifests name (archive ids and
    /// the folder of every shard object, see [`owned_archives`]): the only
    /// data it may own.
    pub owns: BTreeSet<String>,
}

/// Archive folder of a shard object (`remote:root/<folder>/<dir>/<file>`).
fn shard_folder(object: &str) -> Option<&str> {
    let path = object.split_once(':').map_or(object, |(_, path)| path);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    (parts.len() >= 3).then(|| parts[parts.len() - 3])
}

/// Archive folders `manifests` (a generation's content manifests) name:
/// each archive id and the folder of each shard object.
pub(crate) fn owned_archives<'a>(
    manifests: impl IntoIterator<Item = &'a Manifest>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for manifest in manifests {
        out.insert(manifest.archive_id.clone());
        out.extend(
            manifest
                .shards
                .iter()
                .filter_map(|s| shard_folder(&s.object))
                .map(str::to_owned),
        );
    }
    out
}

/// A configured pool whose metadata was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PoolScope {
    /// Pool name.
    pub name: String,
    /// Its pool-sync scope (64 hex).
    pub scope: String,
    /// The pool stores data on this provider.
    pub uses_root: bool,
}

/// Fresh, read-only observation (built by `observe`, faked in tests).
#[derive(Debug, Clone, Default)]
pub(crate) struct World {
    /// Provider root (`remote:path`).
    pub root: String,
    /// Its recursive listing.
    pub listing: Listing,
    /// Configured pools whose metadata was read.
    pub pools: Vec<PoolScope>,
    /// Every generation of every read pool, and of foreign scopes found here.
    pub generations: Vec<Generation>,
    /// Other references that keep data: inventory, manifests on every listed
    /// account, drive uploads no metadata names, migrations in progress.
    pub live: References,
    /// What finished or abandoned migrations name.
    pub leftovers: References,
    /// Archive ids with a manifest replica on any listed account.
    pub manifests: BTreeSet<String>,
    /// Sources that could not be read.
    pub uncertain: Vec<String>,
    /// Current unix time.
    pub now: u64,
}

/// Where one unit belongs and what deleting it involves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placed {
    /// Group id.
    pub group: String,
    /// Objects on the provider, manifest replicas first.
    pub objects: Vec<RetireObject>,
    /// A metadata unit: deleted after the archives in `depends`.
    pub metadata: bool,
    /// Archive units (keys) that must be gone before this unit is deleted.
    pub depends: BTreeSet<String>,
}

impl Placed {
    /// Stored bytes.
    pub(crate) fn bytes(&self) -> u64 {
        self.objects.iter().map(|o| o.size).sum()
    }
}

/// Result of [`classify`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Classified {
    /// Provider root.
    pub root: String,
    /// Configured pools that use this provider.
    pub pools: Vec<String>,
    /// Groups by id.
    pub groups: BTreeMap<String, Group>,
    /// Every unit (key) and its group.
    pub units: BTreeMap<String, Placed>,
    /// Unreadable sources (nothing is deletable while any is listed).
    pub uncertain: Vec<String>,
    /// Everything stored on the provider (mass-delete guard).
    pub totals: Totals,
}

impl Classified {
    /// The group of unit `key`, if the unit exists.
    pub(crate) fn group_of(&self, key: &str) -> Option<&Group> {
        self.units.get(key).and_then(|p| self.groups.get(&p.group))
    }
}

/// An older generation or a foreign pool: what may own deletable data.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    /// An older generation of a configured pool.
    Old {
        /// Pool name.
        pool: String,
        /// Pool-sync scope.
        scope: String,
        /// Epoch (`None`: first generation).
        epoch: Option<String>,
        /// No recorded migration replaced it.
        unconfirmed: bool,
        /// Migration that replaced it, if recorded.
        migration: Option<String>,
    },
    /// A pool not configured on this PC.
    Foreign {
        /// Pool-sync scope.
        scope: String,
    },
}

/// `first` or `epoch <12 hex>`.
fn generation_label(epoch: Option<&str>) -> String {
    match epoch {
        None => "first".into(),
        Some(epoch) => format!("epoch {}", &epoch[..epoch.len().min(12)]),
    }
}

impl Owner {
    /// Stable identity (scope and epoch), independent of status.
    fn key(&self) -> String {
        match self {
            Owner::Old { scope, epoch, .. } => {
                format!("{scope}/{}", epoch.as_deref().unwrap_or("first"))
            }
            Owner::Foreign { scope, .. } => format!("foreign/{scope}"),
        }
    }
    /// Acknowledgement needed to delete what only it names.
    fn needs(&self) -> Option<Ack> {
        match self {
            Owner::Old {
                unconfirmed: true, ..
            } => Some(Ack::UnconfirmedGenerations),
            Owner::Old { .. } => None,
            Owner::Foreign { .. } => Some(Ack::ForeignPools),
        }
    }
    /// Human description for a group reason.
    fn describe(&self) -> String {
        match self {
            Owner::Old {
                pool,
                epoch,
                migration: Some(migration),
                ..
            } => format!(
                "drive generation {} of pool {pool}, replaced by migration {migration}",
                generation_label(epoch.as_deref())
            ),
            Owner::Old { pool, epoch, .. } => format!(
                "drive generation {} of pool {pool}, older than the current one; no recorded migration replaced it, so a PC whose drive was not switched may still use it",
                generation_label(epoch.as_deref())
            ),
            Owner::Foreign { scope } => format!(
                "pool {}… that is not configured on this PC",
                &scope[..12]
            ),
        }
    }
}

/// Group id of an owner set (stable across runs).
fn owners_id(owners: &BTreeSet<Owner>) -> String {
    match owners.iter().collect::<Vec<_>>().as_slice() {
        [Owner::Old { pool, epoch, .. }] => format!(
            "old:{pool}:{}",
            epoch.as_deref().map_or("first", |e| &e[..e.len().min(16)])
        ),
        [Owner::Foreign { scope, .. }] => format!("foreign:{}", &scope[..16]),
        _ => {
            let keys: Vec<String> = owners.iter().map(Owner::key).collect();
            let hash = blake3::hash(keys.join("\n").as_bytes()).to_hex();
            format!("shared:{}", &hash[..16])
        }
    }
}

/// Where a unit goes before groups are built.
enum Target {
    /// A kept group.
    Kept {
        /// Group id.
        id: String,
        /// Category.
        kind: GroupKind,
        /// Configured pool, if any.
        pool: Option<String>,
        /// Reason shown for the group.
        reason: String,
    },
    /// Speed-test leftovers older than a day.
    SpeedTest,
    /// Owned only by older generations / foreign pools.
    Owned(BTreeSet<Owner>),
}

/// A kept target.
fn kept(id: &str, kind: GroupKind, pool: Option<&str>, reason: &str) -> Target {
    Target::Kept {
        id: id.into(),
        kind,
        pool: pool.map(str::to_owned),
        reason: reason.into(),
    }
}

/// The in-use target of `pool` (`None`: other references).
fn in_use(pool: Option<&str>) -> Target {
    match pool {
        Some(pool) => kept(
            &format!("in-use:{pool}"),
            GroupKind::InUse,
            Some(pool),
            &format!("used by pool {pool}: its current drive generation, manifests, migration journals or migrations in progress name this data"),
        ),
        None => kept(
            "in-use",
            GroupKind::InUse,
            None,
            "named by a manifest, this PC's inventory, a drive upload in progress or a migration in progress",
        ),
    }
}

/// The owner of a generation that may own deletable data, if it is one.
fn owner(generation: &Generation) -> Option<Owner> {
    match (&generation.status, &generation.pool) {
        (GenerationStatus::Superseded { migration }, Some(pool)) => Some(Owner::Old {
            pool: pool.clone(),
            scope: generation.scope.clone(),
            epoch: generation.epoch.clone(),
            unconfirmed: false,
            migration: Some(migration.clone()),
        }),
        (GenerationStatus::Unconfirmed, Some(pool)) => Some(Owner::Old {
            pool: pool.clone(),
            scope: generation.scope.clone(),
            epoch: generation.epoch.clone(),
            unconfirmed: true,
            migration: None,
        }),
        (GenerationStatus::Foreign, _) => Some(Owner::Foreign {
            scope: generation.scope.clone(),
        }),
        _ => None,
    }
}

/// Whether the generation's data is in use.
fn live(status: &GenerationStatus) -> bool {
    matches!(
        status,
        GenerationStatus::Current
            | GenerationStatus::Newer
            | GenerationStatus::MigrationInProgress { .. }
    )
}

/// Objects of `unit` on `root`, manifest replicas first.
fn objects(root: &str, unit: &Unit) -> Vec<RetireObject> {
    let mut out: Vec<RetireObject> = unit
        .objects
        .iter()
        .map(|(path, entry)| RetireObject {
            address: crate::utils::remote_join(root, path),
            size: entry.size,
            root: root.into(),
        })
        .collect();
    out.sort_by_key(|o| (!o.address.ends_with("/manifest.json"), o.address.clone()));
    out
}

/// Where an archive folder goes (see the module docs).
fn archive_target(world: &World, id: &str, objects: &[RetireObject]) -> Target {
    let names = |refs: &References| {
        !refs
            .referrers(id, objects.iter().map(|o| o.address.as_str()))
            .is_empty()
    };
    if let Some(generation) = world
        .generations
        .iter()
        .find(|g| live(&g.status) && names(&g.refs))
    {
        return in_use(generation.pool.as_deref());
    }
    if names(&world.live) {
        return in_use(None);
    }
    let leftover = names(&world.leftovers)
        || world.generations.iter().any(|g| {
            matches!(g.status, GenerationStatus::MigrationLeftover { .. }) && names(&g.refs)
        });
    if leftover {
        return migration_leftovers();
    }
    let drive = crate::migration::enumerate::is_drive_archive(id);
    let unknown_put = || {
        kept(
            "unknown-put-archives",
            GroupKind::UnknownPutArchive,
            None,
            "an archive with a manifest that nothing on this PC names (an upload of another PC, or of a pool that is not configured here)",
        )
    };
    if !drive && world.manifests.contains(id) {
        return unknown_put();
    }
    let owners: BTreeSet<Owner> = world
        .generations
        .iter()
        .filter(|g| g.owns.contains(id))
        .filter_map(owner)
        .collect();
    if !owners.is_empty() {
        return Target::Owned(owners);
    }
    kept(
        "incomplete-uploads",
        GroupKind::IncompleteUpload,
        None,
        "shards without a manifest, or a drive upload no drive metadata names yet (it may still be published)",
    )
}

/// The kept target of metadata of pool `pool`, which is configured on this
/// PC without this provider.
fn elsewhere(pool: &str) -> Target {
    kept(
        &format!("configured-elsewhere:{pool}"),
        GroupKind::InUse,
        Some(pool),
        &format!("metadata of pool {pool}, which this PC configures without this provider: a PC configured differently (or an older configuration) may still use it here; only generations this PC's metadata of the pool shows as replaced or older can be deleted"),
    )
}

/// Status of a copy, on this provider, of a generation of a configured pool
/// that does not use the provider (pure): `own` is the generation's status
/// from the pool's own metadata, `newest` the copy's newest write here.
/// Only an older generation (superseded or unconfirmed) not written here in
/// the last [`RECENT_GENERATION`] seconds may own deletable data, and only
/// as unconfirmed (deleted with an acknowledgement); the
/// current one, a newer one, one the pool's metadata does not have and a
/// recent copy are in use.
pub(crate) fn copy_status(
    own: Option<&GenerationStatus>,
    newest: Option<u64>,
    now: u64,
) -> GenerationStatus {
    let recent = newest.is_none_or(|t| t.saturating_add(RECENT_GENERATION) > now);
    match own {
        Some(
            status @ (GenerationStatus::MigrationInProgress { .. }
            | GenerationStatus::MigrationLeftover { .. }),
        ) => status.clone(),
        // Never offered without an acknowledgement: the first generation has
        // no epoch, so a same-name pool another PC uses here shares it.
        Some(GenerationStatus::Superseded { .. } | GenerationStatus::Unconfirmed) if !recent => {
            GenerationStatus::Unconfirmed
        }
        Some(GenerationStatus::Current) => GenerationStatus::Current,
        _ => GenerationStatus::Newer,
    }
}

/// The migration-leftovers target.
fn migration_leftovers() -> Target {
    kept(
        "migration-leftovers",
        GroupKind::MigrationLeftovers,
        None,
        "left by a finished or abandoned migration; clean it up with `rpool pool migrate retire`",
    )
}

/// Where a metadata generation goes. A configured pool's generation (also
/// a copy here of a pool that does not use this provider: its statuses come
/// from the pool's own metadata, see [`copy_status`]) is kept unless every
/// reading of it is an older generation.
fn generation_target(world: &World, scope: &str, epoch: Option<&str>) -> Target {
    let Some(pool) = world.pools.iter().find(|p| p.scope == scope) else {
        return Target::Owned(
            [Owner::Foreign {
                scope: scope.into(),
            }]
            .into(),
        );
    };
    let keep = || {
        if pool.uses_root {
            in_use(Some(&pool.name))
        } else {
            elsewhere(&pool.name)
        }
    };
    let found: Vec<&Generation> = world
        .generations
        .iter()
        .filter(|g| g.scope == scope && g.epoch.as_deref() == epoch)
        .collect();
    if found.is_empty() || found.iter().any(|g| live(&g.status)) {
        return keep();
    }
    if found
        .iter()
        .any(|g| matches!(g.status, GenerationStatus::MigrationLeftover { .. }))
    {
        return migration_leftovers();
    }
    let owners: Option<BTreeSet<Owner>> = found.into_iter().map(owner).collect();
    match owners {
        Some(owners) => Target::Owned(owners),
        None => keep(),
    }
}

/// Where a unit goes.
fn target(world: &World, unit: &Unit, objects: &[RetireObject]) -> Target {
    match &unit.kind {
        UnitKind::Archive { id } => archive_target(world, id, objects),
        UnitKind::Generation { scope, epoch } => generation_target(world, scope, epoch.as_deref()),
        UnitKind::Migration { scope, .. } => {
            match world.pools.iter().find(|p| &p.scope == scope) {
                Some(pool) if pool.uses_root => in_use(Some(&pool.name)),
                Some(pool) => elsewhere(&pool.name),
                None => Target::Owned(
                    [Owner::Foreign {
                        scope: scope.clone(),
                    }]
                    .into(),
                ),
            }
        }
        UnitKind::SpeedTest { .. } | UnitKind::Bench { .. } => match unit.newest() {
            Some(newest) if newest.saturating_add(SPEED_TEST_AGE) <= world.now => {
                Target::SpeedTest
            }
            _ => kept(
                "speed-test-recent",
                GroupKind::SpeedTestLeftovers,
                None,
                "speed-test files from the last 24 hours: a test may still be running",
            ),
        },
        UnitKind::Journal => kept(
            "storage-cleanup-journal",
            GroupKind::Journal,
            None,
            "this manager's marks; other PCs read them",
        ),
        UnitKind::Other { .. } => kept(
            "not-rpool",
            GroupKind::NotRpool,
            None,
            "not in RPool's storage layout (other files, or folders with files an archive never has)",
        ),
    }
}

/// An empty group.
fn group(id: &str, kind: GroupKind, pool: Option<String>, reason: String) -> Group {
    Group {
        id: id.into(),
        kind,
        pool,
        scope: None,
        generations: Vec::new(),
        units: 0,
        archives: 0,
        objects: 0,
        bytes: 0,
        deletable: false,
        needs: Vec::new(),
        grace_seconds: None,
        reason,
        pending: Pending::default(),
    }
}

/// The group of an owner set.
fn owned_group(id: &str, owners: &BTreeSet<Owner>) -> Group {
    let all_foreign = owners.iter().all(|o| matches!(o, Owner::Foreign { .. }));
    let kind = if all_foreign {
        GroupKind::ForeignPool
    } else {
        GroupKind::OldGeneration
    };
    let pools: BTreeSet<&str> = owners
        .iter()
        .filter_map(|o| match o {
            Owner::Old { pool, .. } => Some(pool.as_str()),
            Owner::Foreign { .. } => None,
        })
        .collect();
    let scopes: BTreeSet<&str> = owners
        .iter()
        .map(|o| match o {
            Owner::Old { scope, .. } | Owner::Foreign { scope, .. } => scope.as_str(),
        })
        .collect();
    let descriptions: Vec<String> = owners.iter().map(Owner::describe).collect();
    let reason = if owners.len() == 1 {
        format!("named only by {}", descriptions[0])
    } else {
        format!(
            "named only by these, so select it together with them: {}",
            descriptions.join(" | ")
        )
    };
    let mut out = group(
        id,
        kind,
        (pools.len() == 1)
            .then(|| pools.iter().next().map(|p| p.to_string()))
            .flatten(),
        reason,
    );
    out.scope = (scopes.len() == 1)
        .then(|| scopes.iter().next().map(|s| s.to_string()))
        .flatten();
    out.generations = owners
        .iter()
        .filter_map(|o| match o {
            Owner::Old { epoch, .. } => Some(generation_label(epoch.as_deref())),
            Owner::Foreign { .. } => None,
        })
        .collect();
    out.deletable = true;
    out.needs = owners
        .iter()
        .filter_map(Owner::needs)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    out
}

/// Groups every unit of `world.listing` (see the module docs).
pub(crate) fn classify(world: &World) -> Classified {
    let mut out = Classified {
        root: world.root.clone(),
        pools: world
            .pools
            .iter()
            .filter(|p| p.uses_root)
            .map(|p| p.name.clone())
            .collect(),
        uncertain: world.uncertain.clone(),
        ..Default::default()
    };
    out.uncertain.extend(world.live.uncertain.iter().cloned());
    out.uncertain
        .extend(world.leftovers.uncertain.iter().cloned());
    for generation in &world.generations {
        out.uncertain
            .extend(generation.refs.uncertain.iter().cloned());
    }
    for entry in world.listing.values() {
        out.totals.objects += 1;
        out.totals.bytes += entry.size;
    }
    let units = layout::parse(&world.listing);
    // Owner -> archive units it names (metadata waits for them).
    let mut owned_archives: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut metadata_owner: BTreeMap<String, String> = BTreeMap::new();
    for (key, unit) in &units {
        let objects = objects(&world.root, unit);
        let metadata = matches!(
            unit.kind,
            UnitKind::Generation { .. } | UnitKind::Migration { .. }
        );
        let id = match target(world, unit, &objects) {
            Target::Kept {
                id,
                kind,
                pool,
                reason,
            } => {
                out.groups
                    .entry(id.clone())
                    .or_insert_with(|| group(&id, kind, pool, reason));
                id
            }
            Target::SpeedTest => {
                let id = "speed-test-leftovers".to_string();
                out.groups.entry(id.clone()).or_insert_with(|| {
                    let mut g = group(
                        &id,
                        GroupKind::SpeedTestLeftovers,
                        None,
                        "files of interrupted speed tests, older than 24 hours (deleted one hour after marking)".into(),
                    );
                    g.deletable = true;
                    g.grace_seconds = Some(SPEED_TEST_GRACE);
                    g
                });
                id
            }
            Target::Owned(owners) => {
                let id = owners_id(&owners);
                for owner in &owners {
                    if metadata {
                        metadata_owner.insert(key.clone(), owner.key());
                    } else {
                        owned_archives
                            .entry(owner.key())
                            .or_default()
                            .insert(key.clone());
                    }
                }
                out.groups
                    .entry(id.clone())
                    .or_insert_with(|| owned_group(&id, &owners));
                id
            }
        };
        let group = out.groups.get_mut(&id).expect("group just inserted");
        group.units += 1;
        group.objects += objects.len() as u64;
        group.bytes += unit.bytes();
        if matches!(unit.kind, UnitKind::Archive { .. }) {
            group.archives += 1;
        }
        out.units.insert(
            key.clone(),
            Placed {
                group: id,
                objects,
                metadata,
                depends: BTreeSet::new(),
            },
        );
    }
    for (key, owner) in metadata_owner {
        if let (Some(placed), Some(archives)) =
            (out.units.get_mut(&key), owned_archives.get(&owner))
        {
            placed.depends = archives.clone();
        }
    }
    if !out.uncertain.is_empty() {
        for group in out.groups.values_mut() {
            if group.deletable {
                group.deletable = false;
                group.reason = format!(
                    "{} (not deletable now: some sources could not be read)",
                    group.reason
                );
            }
        }
    }
    out
}

/// What the drive migration records `known` of pool `pool` cannot explain
/// (pure); each gap makes the view uncertain, as statuses would rest on
/// partial knowledge: a migration journal `listed` on the pool's accounts
/// that `known` lacks, an adoption of a `found` generation whose new
/// generation was neither found nor replaced in turn, or found generations
/// none of which a reader may use.
pub(crate) fn knowledge_gaps(
    pool: &str,
    found: &[crate::pool::browse_generations::Generation],
    known: &crate::migration::drive_generations::Known,
    listed: &BTreeSet<String>,
) -> Vec<String> {
    use crate::migration::drive_generations::{effective, generation_ref};
    let mut out: Vec<String> = listed
        .difference(&known.migration_ids)
        .map(|id| {
            format!("migration {id} of pool {pool}: listed, but its records could not be read")
        })
        .collect();
    let refs: BTreeSet<_> = found.iter().map(generation_ref).collect();
    for adoption in &known.adoptions {
        let target = adoption.generation();
        let replaced = known.adoptions.iter().any(|a| a.source == target);
        if refs.contains(&adoption.source) && !refs.contains(&target) && !replaced {
            out.push(format!(
                "pool {pool}: migration {} adopted drive generation {}, which was not found",
                adoption.migration_id,
                generation_label(Some(&adoption.epoch))
            ));
        }
    }
    if !found.is_empty() && effective(found.to_vec(), known).is_empty() {
        out.push(format!(
            "pool {pool}: none of its drive generations is usable according to its migration records"
        ));
    }
    out
}

/// Status of every generation of one configured pool (pure): superseded by
/// a recorded adoption, a migration's own generation (adopted, in progress
/// or left over), the current one (`drive_generations::effective` order),
/// newer than the current one or written recently, or an unconfirmed older
/// one. `abandoned`: abandoned migrations (any other is still running);
/// `now`: unix seconds.
pub(crate) fn generation_statuses(
    generations: &[crate::pool::browse_generations::Generation],
    known: &crate::migration::drive_generations::Known,
    abandoned: &BTreeSet<String>,
    now: u64,
) -> Vec<(Option<String>, GenerationStatus)> {
    let recent = i128::from(now.saturating_sub(RECENT_GENERATION)) * 1_000_000_000;
    use crate::migration::drive_generations::{effective, generation_ref};
    use crate::migration::drive_model::epoch_for;
    let effective = effective(generations.to_vec(), known);
    let current = effective.first();
    let migration_epochs: BTreeMap<String, &String> = known
        .migration_ids
        .iter()
        .map(|id| (epoch_for(id), id))
        .collect();
    generations
        .iter()
        .map(|g| {
            let reference = generation_ref(g);
            let adoption = known
                .adoptions
                .iter()
                .filter(|a| a.source == reference)
                .max_by(|a, b| (a.ts_unix, &a.epoch).cmp(&(b.ts_unix, &b.epoch)));
            let adopted = g
                .epoch
                .as_deref()
                .is_some_and(|e| known.adoptions.iter().any(|a| a.epoch == e));
            let partial = g
                .epoch
                .as_deref()
                .and_then(|e| migration_epochs.get(e))
                .filter(|_| !adopted);
            let status = if let Some(adoption) = adoption {
                GenerationStatus::Superseded {
                    migration: adoption.migration_id.clone(),
                }
            } else if let Some(migration) = partial {
                if abandoned.contains(*migration) {
                    GenerationStatus::MigrationLeftover {
                        migration: (*migration).clone(),
                    }
                } else {
                    GenerationStatus::MigrationInProgress {
                        migration: (*migration).clone(),
                    }
                }
            } else if current.is_some_and(|c| c.epoch == g.epoch) {
                GenerationStatus::Current
            } else if current.is_some_and(|c| g.newest >= c.newest) || g.newest >= recent {
                GenerationStatus::Newer
            } else {
                GenerationStatus::Unconfirmed
            };
            (g.epoch.clone(), status)
        })
        .collect()
}
