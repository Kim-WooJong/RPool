//! Layout, classification and cleanup-journal tests over synthetic
//! listings and an in-memory `StorageIo` (no rclone).
use super::classify::{
    classify, generation_statuses, Classified, Generation, GenerationStatus, PoolScope, World,
    SPEED_TEST_AGE, SPEED_TEST_GRACE,
};
use super::execute::{run, view, Request, StorageIo};
use super::layout::{self, Entry, UnitKind};
use super::model::{Ack, CleanMode, GroupKind};
use crate::drive_history::cleanup::records::{self, Record, Step};
use crate::migration::retire::refs::References;
use crate::prelude::*;

const ROOT: &str = "r:pool";
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn scope(name: &str) -> String {
    crate::mount::pool_sync::scope(name).unwrap()
}

fn epoch(c: char) -> String {
    c.to_string().repeat(64)
}

fn entry(size: u64, modified: u64) -> Entry {
    Entry {
        size,
        modified: Some(modified),
    }
}

/// An archive folder with a manifest and `shards` shards of 10 bytes.
fn archive(world: &mut World, id: &str, shards: usize) {
    world
        .listing
        .insert(format!("{id}/manifest.json"), entry(1, NOW - 100 * DAY));
    for i in 0..shards {
        world
            .listing
            .insert(format!("{id}/data/{i:08}.bin"), entry(10, NOW - 100 * DAY));
    }
    world.manifests.insert(id.into());
}

/// Metadata of generation `epoch` of `scope` on the provider.
fn metadata(world: &mut World, scope: &str, epoch: Option<&str>) {
    let dir = match epoch {
        Some(e) => format!(".rpool-sync/events-v6/{scope}/epochs/{e}"),
        None => format!(".rpool-sync/events-v6/{scope}"),
    };
    world.listing.insert(
        format!("{dir}/events/{}.json", "e".repeat(64)),
        entry(5, NOW - 50 * DAY),
    );
}

/// Events naming `ids` (as a drive event names its content's archive).
fn names(ids: &[&str]) -> References {
    let mut refs = References::default();
    for id in ids {
        refs.add_json(
            "events",
            &serde_json::json!({"content": {"manifest": {
                "archive_id": id,
                "shards": [{"object": format!("{ROOT}/{id}/data/00000000.bin")}]
            }}}),
        );
    }
    refs
}

fn generation(
    pool: Option<&str>,
    epoch: Option<&str>,
    status: GenerationStatus,
    ids: &[&str],
) -> Generation {
    Generation {
        pool: pool.map(str::to_owned),
        scope: pool.map_or_else(|| "f".repeat(64), scope),
        epoch: epoch.map(str::to_owned),
        status,
        refs: names(ids),
        owns: ids.iter().map(|id| id.to_string()).collect(),
    }
}

/// Makes `generation`'s events name (and own) exactly `ids`.
fn rename(generation: &mut Generation, ids: &[&str]) {
    generation.refs = names(ids);
    generation.owns = ids.iter().map(|id| id.to_string()).collect();
}

/// Pool "RPool" on this provider: current generation `c` naming `live`,
/// superseded first generation naming `old`, unconfirmed epoch `a` naming
/// `transition`.
fn world() -> World {
    let mut world = World {
        root: ROOT.into(),
        now: NOW,
        pools: vec![PoolScope {
            name: "RPool".into(),
            scope: scope("RPool"),
            uses_root: true,
        }],
        ..Default::default()
    };
    for id in ["virtual-live", "virtual-old", "virtual-both", "virtual-tr"] {
        archive(&mut world, id, 2);
    }
    let s = scope("RPool");
    metadata(&mut world, &s, None);
    metadata(&mut world, &s, Some(&epoch('a')));
    metadata(&mut world, &s, Some(&epoch('c')));
    world.generations = vec![
        generation(
            Some("RPool"),
            Some(&epoch('c')),
            GenerationStatus::Current,
            &["virtual-live", "virtual-both"],
        ),
        generation(
            Some("RPool"),
            None,
            GenerationStatus::Superseded {
                migration: "m1".into(),
            },
            &["virtual-old", "virtual-both"],
        ),
        generation(
            Some("RPool"),
            Some(&epoch('a')),
            GenerationStatus::Unconfirmed,
            &["virtual-tr"],
        ),
    ];
    world
}

fn group_of<'a>(c: &'a Classified, key: &str) -> &'a super::model::Group {
    c.group_of(key)
        .unwrap_or_else(|| panic!("{key} not placed"))
}

#[test]
fn layout_parses_every_kind_and_keeps_strange_folders_whole() {
    let s = "1".repeat(64);
    let listing: layout::Listing = [
        "virtual-a/manifest.json",
        "virtual-a/data/00000000.bin",
        "virtual-a/parity/g00000000-p000.bin",
        "put-0123456789abcdef/shards/00000000.bin",
        "mixed/manifest.json",
        "mixed/notes.txt",
        &format!(".rpool-sync/events-v6/{s}/events/x.json"),
        &format!(".rpool-sync/events-v6/{s}/epochs/{s}/events/y.json"),
        &format!(".rpool-sync/events-v6/{s}/epochs/bad/events/y.json"),
        &format!(".rpool-sync/migrations-v1/{s}/m1/plan.json"),
        ".rpool-sync/bench/x.bin",
        ".rpool-sync/storage-cleanup-v1/z.json",
        ".rpool-sync/other/q",
        ".rpool-speedtest/run1/f0",
        "loose.txt",
    ]
    .iter()
    .map(|p| (p.to_string(), entry(1, 1)))
    .collect();
    let units = layout::parse(&listing);
    let kinds: BTreeMap<&str, &UnitKind> =
        units.iter().map(|(k, u)| (k.as_str(), &u.kind)).collect();
    assert_eq!(units["archive:virtual-a"].objects.len(), 3);
    assert!(kinds.contains_key("archive:put-0123456789abcdef"));
    assert_eq!(
        units["other:mixed"].objects.len(),
        2,
        "a folder with foreign files stays whole"
    );
    assert!(!kinds.contains_key("archive:mixed"));
    assert!(kinds.contains_key(format!("generation:{s}/first").as_str()));
    assert!(kinds.contains_key(format!("generation:{s}/{s}").as_str()));
    assert_eq!(units["other:.rpool-sync"].objects.len(), 2);
    assert!(kinds.contains_key(format!("migration:{s}/m1").as_str()));
    assert!(kinds.contains_key("bench:x.bin"));
    assert!(kinds.contains_key("journal"));
    assert!(kinds.contains_key("speedtest:run1"));
    assert!(kinds.contains_key("other:loose.txt"));
    assert_eq!(
        layout::parents("a/b/c.bin").collect::<Vec<_>>(),
        vec!["a/b", "a"]
    );
    let parsed = layout::parse_lsjson(
        br#"[{"Path":"d","IsDir":true,"Size":-1},{"Path":"d/f","Size":3,"ModTime":"1970-01-02T00:00:00Z"}]"#,
    )
    .unwrap();
    assert_eq!(parsed["d/f"], entry(3, DAY));
    assert_eq!(parsed.len(), 1);
}

#[test]
fn objects_of_unknown_size_are_kept_and_make_a_folder_other() {
    let parsed = layout::parse_lsjson(
        br#"[{"Path":"virtual-a/manifest.json","Size":1},
            {"Path":"virtual-a/notes","Size":-1},
            {"Path":"loose","Size":-1}]"#,
    )
    .unwrap();
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed["virtual-a/notes"].size, 0);
    let units = layout::parse(&parsed);
    assert!(!units.contains_key("archive:virtual-a"));
    assert_eq!(units["other:virtual-a"].objects.len(), 2);
    assert!(units.contains_key("other:loose"));
}

#[test]
fn archives_are_grouped_by_what_names_them() {
    let c = classify(&world());
    let s = scope("RPool");
    // Named by the current generation (even if also by an old one): in use.
    for id in ["virtual-live", "virtual-both"] {
        let g = group_of(&c, &format!("archive:{id}"));
        assert_eq!((g.kind, g.deletable), (GroupKind::InUse, false), "{id}");
    }
    let current = group_of(&c, &format!("generation:{s}/{}", epoch('c')));
    assert_eq!(current.kind, GroupKind::InUse);
    // Superseded generation: its own data and metadata, deletable freely.
    let old = group_of(&c, "archive:virtual-old");
    assert_eq!(old.kind, GroupKind::OldGeneration);
    assert!(old.deletable && old.needs.is_empty());
    assert_eq!(old.id, "old:RPool:first");
    assert_eq!(group_of(&c, &format!("generation:{s}/first")).id, old.id);
    assert_eq!((old.units, old.archives, old.objects), (2, 1, 4));
    // The metadata waits for the archive only it names.
    let placed = &c.units[&format!("generation:{s}/first")];
    assert!(placed.metadata);
    assert_eq!(placed.depends, ["archive:virtual-old".to_string()].into());
    // Unconfirmed transition generation: only with its acknowledgement.
    let tr = group_of(&c, "archive:virtual-tr");
    assert_eq!(tr.kind, GroupKind::OldGeneration);
    assert_eq!(tr.needs, vec![Ack::UnconfirmedGenerations]);
    assert!(c.uncertain.is_empty());
}

#[test]
fn a_reference_from_another_account_keeps_an_object_in_use() {
    let mut world = world();
    // A put archive on another account borrows one object of virtual-old.
    let mut other = crate::migration::test_support::manifest("put-x", 10, 4, 2, 1, &["b:pool"]);
    other.shards[0].object = format!("{ROOT}/virtual-old/data/00000001.bin");
    world
        .live
        .add_manifest("manifest of put-x on b:pool", &other);
    let c = classify(&world);
    assert_eq!(group_of(&c, "archive:virtual-old").kind, GroupKind::InUse);
    // Its own manifest is not a reference to itself.
    let mut own = References::default();
    own.add_manifest(
        "manifest",
        &crate::migration::test_support::manifest("virtual-tr", 10, 4, 2, 1, &[ROOT]),
    );
    let mut world2 = self::world();
    world2.live = own;
    assert_eq!(
        group_of(&classify(&world2), "archive:virtual-tr").kind,
        GroupKind::OldGeneration
    );
}

#[test]
fn shared_old_data_needs_every_owner() {
    let mut world = world();
    archive(&mut world, "virtual-shared", 1);
    rename(
        &mut world.generations[1],
        &["virtual-old", "virtual-both", "virtual-shared"],
    );
    rename(&mut world.generations[2], &["virtual-tr", "virtual-shared"]);
    let c = classify(&world);
    let shared = group_of(&c, "archive:virtual-shared");
    assert!(shared.id.starts_with("shared:"));
    assert_eq!(shared.kind, GroupKind::OldGeneration);
    assert_eq!(shared.needs, vec![Ack::UnconfirmedGenerations]);
    assert_eq!(shared.generations.len(), 2);
    // Both generations' metadata wait for it.
    let s = scope("RPool");
    for e in ["first".to_string(), epoch('a')] {
        assert!(c.units[&format!("generation:{s}/{e}")]
            .depends
            .contains("archive:virtual-shared"));
    }
}

#[test]
fn foreign_pools_and_migration_leftovers() {
    let mut world = world();
    let foreign = "f".repeat(64);
    metadata(&mut world, &foreign, None);
    world.listing.insert(
        format!(".rpool-sync/migrations-v1/{foreign}/m9/plan.json"),
        entry(1, 1),
    );
    archive(&mut world, "virtual-foreign", 1);
    world.generations.push(generation(
        None,
        None,
        GenerationStatus::Foreign,
        &["virtual-foreign"],
    ));
    archive(&mut world, "virtual-left", 1);
    world.leftovers.add_text("migration m2", "virtual-left");
    let s = scope("RPool");
    world.listing.insert(
        format!(".rpool-sync/migrations-v1/{s}/m1/plan.json"),
        entry(1, 1),
    );
    let c = classify(&world);
    let g = group_of(&c, "archive:virtual-foreign");
    assert_eq!(g.kind, GroupKind::ForeignPool);
    assert_eq!(g.needs, vec![Ack::ForeignPools]);
    assert_eq!(g.id, format!("foreign:{}", &foreign[..16]));
    assert_eq!(
        group_of(&c, &format!("generation:{foreign}/first")).id,
        g.id
    );
    assert_eq!(group_of(&c, &format!("migration:{foreign}/m9")).id, g.id);
    assert_eq!(g.units, 3);
    // The configured pool's own migration journal is in use.
    assert_eq!(
        group_of(&c, &format!("migration:{s}/m1")).kind,
        GroupKind::InUse
    );
    let left = group_of(&c, "archive:virtual-left");
    assert_eq!(
        (left.kind, left.deletable),
        (GroupKind::MigrationLeftovers, false)
    );
    assert!(left.reason.contains("pool migrate retire"));
}

#[test]
fn a_configured_pool_without_this_provider_keeps_what_its_metadata_uses() {
    let mut world = world();
    world.pools.push(PoolScope {
        name: "Other".into(),
        scope: scope("Other"),
        uses_root: false,
    });
    let s = scope("Other");
    // Copies here of its first (superseded), epoch d (current), epoch e
    // (unconfirmed) and epoch f (unknown to its metadata) generations, and
    // a migration journal.
    for e in [None, Some(epoch('d')), Some(epoch('e')), Some(epoch('f'))] {
        metadata(&mut world, &s, e.as_deref());
    }
    world.listing.insert(
        format!(".rpool-sync/migrations-v1/{s}/m7/plan.json"),
        entry(1, 1),
    );
    let other = |epoch: Option<&str>, status: GenerationStatus, ids: &[&str]| {
        let mut g = generation(Some("Other"), epoch, status, ids);
        g.scope = s.clone();
        g
    };
    for id in ["virtual-o-old", "virtual-o-cur", "virtual-o-tr"] {
        archive(&mut world, id, 1);
    }
    world.generations.extend([
        other(
            None,
            GenerationStatus::Superseded {
                migration: "m7".into(),
            },
            &["virtual-o-old"],
        ),
        other(
            Some(&epoch('d')),
            GenerationStatus::Current,
            &["virtual-o-cur"],
        ),
        other(
            Some(&epoch('e')),
            GenerationStatus::Unconfirmed,
            &["virtual-o-tr"],
        ),
    ]);
    let c = classify(&world);
    let gen = |e: &str| group_of(&c, &format!("generation:{s}/{e}"));
    // Current and unknown generations and the migration journal are kept.
    for g in [gen(&epoch('d')), gen(&epoch('f'))] {
        assert_eq!((g.kind, g.deletable), (GroupKind::InUse, false), "{g:?}");
        assert_eq!(g.pool.as_deref(), Some("Other"));
        assert!(!g.reason.contains("no longer uses"), "{}", g.reason);
    }
    let journal = group_of(&c, &format!("migration:{s}/m7"));
    assert!(!journal.deletable && journal.kind == GroupKind::InUse);
    assert!(!group_of(&c, "archive:virtual-o-cur").deletable);
    // Older generations follow the pool's own metadata, with their acknowledgement.
    let old = gen("first");
    assert_eq!(
        (old.kind, old.deletable, old.needs.clone()),
        (GroupKind::OldGeneration, true, vec![])
    );
    assert_eq!(old.id, "old:Other:first");
    assert_eq!(group_of(&c, "archive:virtual-o-old").id, old.id);
    let tr = gen(&epoch('e'));
    assert_eq!(tr.needs, vec![Ack::UnconfirmedGenerations]);
    // Two readings of one generation (its own metadata and the copy here):
    // kept while either is in use.
    world
        .generations
        .push(other(None, GenerationStatus::Newer, &["virtual-o-old"]));
    let c = classify(&world);
    assert!(!group_of(&c, &format!("generation:{s}/first")).deletable);
    assert!(!group_of(&c, "archive:virtual-o-old").deletable);
}

#[test]
fn a_copy_takes_its_pools_status_unless_written_recently() {
    use super::classify::{copy_status, RECENT_GENERATION};
    let old = NOW - RECENT_GENERATION - 1;
    let superseded = GenerationStatus::Superseded {
        migration: "m".into(),
    };
    assert_eq!(copy_status(Some(&superseded), Some(old), NOW), superseded);
    assert_eq!(
        copy_status(Some(&GenerationStatus::Unconfirmed), Some(old), NOW),
        GenerationStatus::Unconfirmed
    );
    // Written here recently, or at an unknown time: in use.
    for newest in [Some(NOW - DAY), None] {
        assert_eq!(
            copy_status(Some(&superseded), newest, NOW),
            GenerationStatus::Newer
        );
    }
    // Current, newer or unknown to the pool's metadata: in use.
    assert_eq!(
        copy_status(Some(&GenerationStatus::Current), Some(old), NOW),
        GenerationStatus::Current
    );
    for own in [Some(&GenerationStatus::Newer), None] {
        assert_eq!(copy_status(own, Some(old), NOW), GenerationStatus::Newer);
    }
}

#[test]
fn unnamed_put_archives_and_incomplete_uploads_are_kept() {
    let mut world = world();
    archive(&mut world, "photo.jpg-0123456789abcdef", 2);
    archive(&mut world, "virtual-unpublished", 2);
    world.listing.insert(
        "half-0123456789abcdef/data/00000000.bin".into(),
        entry(1, 1),
    );
    let c = classify(&world);
    let put = group_of(&c, "archive:photo.jpg-0123456789abcdef");
    assert_eq!(
        (put.kind, put.deletable),
        (GroupKind::UnknownPutArchive, false)
    );
    for key in [
        "archive:virtual-unpublished",
        "archive:half-0123456789abcdef",
    ] {
        let g = group_of(&c, key);
        assert_eq!(
            (g.kind, g.deletable),
            (GroupKind::IncompleteUpload, false),
            "{key}"
        );
    }
    // In this PC's inventory: in use.
    world
        .live
        .add_text("this PC's inventory", "photo.jpg-0123456789abcdef");
    assert_eq!(
        group_of(&classify(&world), "archive:photo.jpg-0123456789abcdef").kind,
        GroupKind::InUse
    );
}

#[test]
fn speed_test_leftovers_older_than_a_day_have_a_short_grace() {
    let mut world = world();
    world.listing.insert(
        ".rpool-speedtest/old/f".into(),
        entry(7, NOW - SPEED_TEST_AGE - 1),
    );
    world
        .listing
        .insert(".rpool-speedtest/new/f".into(), entry(7, NOW - 60));
    world
        .listing
        .insert(".rpool-sync/bench/x.bin".into(), entry(7, NOW - 2 * DAY));
    world.listing.insert(
        ".rpool-speedtest/unknown/f".into(),
        Entry {
            size: 1,
            modified: None,
        },
    );
    let c = classify(&world);
    let old = group_of(&c, "speedtest:old");
    assert!(old.deletable && old.needs.is_empty());
    assert_eq!(old.grace_seconds, Some(SPEED_TEST_GRACE));
    assert_eq!(group_of(&c, "bench:x.bin").id, old.id);
    for key in ["speedtest:new", "speedtest:unknown"] {
        assert!(!group_of(&c, key).deletable, "{key}");
    }
}

#[test]
fn any_uncertain_source_makes_nothing_deletable() {
    for spoil in 0..4 {
        let mut world = world();
        match spoil {
            0 => world.uncertain.push("b:pool: listing failed".into()),
            1 => world.live.uncertain("manifest of x".into()),
            2 => world.leftovers.uncertain("migration m".into()),
            _ => world.generations[1].refs.uncertain("events".into()),
        }
        let c = classify(&world);
        assert!(!c.uncertain.is_empty());
        assert!(c.groups.values().all(|g| !g.deletable), "{spoil}");
    }
}

#[test]
fn generation_status_superseded_transition_partial_and_newer() {
    use crate::migration::drive_generations::Known;
    use crate::migration::drive_model::{epoch_for, DriveAdoption, GenerationRef};
    use crate::pool::browse_generations::Generation as Found;
    let found = |epoch: Option<String>, newest: i128| Found {
        epoch,
        newest,
        records: 1,
    };
    let adopted = epoch_for("mig-a");
    let partial = epoch_for("mig-b");
    let left = epoch_for("mig-c");
    let generations = vec![
        found(Some(epoch('9')), 60),
        found(Some(left.clone()), 50),
        found(Some(partial.clone()), 40),
        found(Some(adopted.clone()), 30),
        found(Some(epoch('2')), 20),
        found(None, 10),
    ];
    let known = Known {
        adoptions: vec![DriveAdoption {
            version: 1,
            migration_id: "mig-a".into(),
            source: GenerationRef { epoch: None },
            epoch: adopted.clone(),
            files: 1,
            dropped: vec![],
            pc_id: "pc".into(),
            ts_unix: 1,
        }],
        freezes: vec![],
        migration_ids: ["mig-a", "mig-b", "mig-c"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let statuses: BTreeMap<Option<String>, GenerationStatus> =
        generation_statuses(&generations, &known, &["mig-c".to_string()].into(), NOW)
            .into_iter()
            .collect();
    // The adopted generation is current; the first one was replaced by it.
    assert_eq!(statuses[&Some(adopted)], GenerationStatus::Current);
    assert_eq!(
        statuses[&None],
        GenerationStatus::Superseded {
            migration: "mig-a".into()
        }
    );
    // A same-name transition newer than the current one is in use; an older one is unconfirmed.
    assert_eq!(statuses[&Some(epoch('9'))], GenerationStatus::Newer);
    assert_eq!(statuses[&Some(epoch('2'))], GenerationStatus::Unconfirmed);
    assert_eq!(
        statuses[&Some(partial)],
        GenerationStatus::MigrationInProgress {
            migration: "mig-b".into()
        }
    );
    assert_eq!(
        statuses[&Some(left)],
        GenerationStatus::MigrationLeftover {
            migration: "mig-c".into()
        }
    );
    // Without migrations the newest generation is current, older ones unconfirmed.
    let plain = generation_statuses(
        &[found(Some(epoch('3')), 5), found(None, 1)],
        &Known::default(),
        &BTreeSet::new(),
        NOW,
    );
    assert_eq!(plain[0].1, GenerationStatus::Current);
    assert_eq!(plain[1].1, GenerationStatus::Unconfirmed);
    // An older generation written in the last two weeks is kept as in use.
    let at = |days: u64| i128::from(NOW - days * DAY) * 1_000_000_000;
    let recent = generation_statuses(
        &[
            found(Some(epoch('3')), at(1)),
            found(None, at(13)),
            found(Some(epoch('4')), at(15)),
        ],
        &Known::default(),
        &BTreeSet::new(),
        NOW,
    );
    assert_eq!(recent[1].1, GenerationStatus::Newer);
    assert_eq!(recent[2].1, GenerationStatus::Unconfirmed);
}

/// In-memory provider: the world changes as objects are deleted.
struct Fake {
    world: Mutex<World>,
    records: Mutex<Vec<Record>>,
    deleted: Mutex<Vec<String>>,
    removed_dirs: Mutex<Vec<String>>,
    clock: Mutex<u64>,
    ids: Mutex<u64>,
    /// Make the observation uncertain from this observe call on (0-based).
    spoil_after: Mutex<Option<usize>>,
    observed: Mutex<usize>,
    fail_delete: Mutex<Option<String>>,
    /// Make observe calls fail from this one on (0-based).
    fail_observe_after: Mutex<Option<usize>>,
}

impl Fake {
    fn new(world: World) -> Self {
        Self {
            world: Mutex::new(world),
            records: Mutex::new(vec![]),
            deleted: Mutex::new(vec![]),
            removed_dirs: Mutex::new(vec![]),
            clock: Mutex::new(NOW),
            ids: Mutex::new(0),
            spoil_after: Mutex::new(None),
            observed: Mutex::new(0),
            fail_delete: Mutex::new(None),
            fail_observe_after: Mutex::new(None),
        }
    }
    fn advance(&self, seconds: u64) {
        *self.clock.lock().unwrap() += seconds;
    }
    fn deleted(&self) -> Vec<String> {
        self.deleted.lock().unwrap().clone()
    }
    fn state(&self) -> BTreeMap<String, records::State> {
        records::fold(&self.records.lock().unwrap())
    }
}

impl StorageIo for Fake {
    fn root(&self) -> String {
        ROOT.into()
    }
    fn records(&self) -> Result<Vec<Record>> {
        Ok(self.records.lock().unwrap().clone())
    }
    fn publish(&self, record: &Record) -> Result<()> {
        self.records.lock().unwrap().push(record.clone());
        Ok(())
    }
    fn observe(&self) -> Result<Classified> {
        let mut world = self.world.lock().unwrap().clone();
        world.now = self.now();
        let mut observed = self.observed.lock().unwrap();
        if self
            .fail_observe_after
            .lock()
            .unwrap()
            .is_some_and(|n| *observed >= n)
        {
            *observed += 1;
            bail!("listing timed out");
        }
        if self
            .spoil_after
            .lock()
            .unwrap()
            .is_some_and(|n| *observed >= n)
        {
            world.uncertain.push("b:pool: listing failed".into());
        }
        *observed += 1;
        Ok(classify(&world))
    }
    fn delete(&self, address: &str) -> Result<()> {
        if self.fail_delete.lock().unwrap().as_deref() == Some(address) {
            bail!("provider refused");
        }
        let rel = crate::utils::relative_remote_object(ROOT, address)?;
        self.world.lock().unwrap().listing.remove(&rel);
        self.deleted.lock().unwrap().push(address.into());
        Ok(())
    }
    fn remove_dir(&self, address: &str) {
        self.removed_dirs.lock().unwrap().push(address.into());
    }
    fn now(&self) -> u64 {
        *self.clock.lock().unwrap()
    }
    fn new_id(&self) -> Result<String> {
        let mut ids = self.ids.lock().unwrap();
        *ids += 1;
        Ok(format!("id{ids}"))
    }
    fn worker(&self) -> String {
        "pc".into()
    }
    fn grace_days(&self) -> u32 {
        7
    }
    fn workers(&self) -> usize {
        3
    }
}

fn request(groups: &[&str], confirm: bool) -> Request {
    Request {
        groups: groups.iter().map(|g| g.to_string()).collect(),
        confirm,
        workers: 2,
        ..Default::default()
    }
}

#[test]
fn preview_writes_nothing_and_selection_is_checked() {
    let fake = Fake::new(world());
    let report = run(&fake, &request(&["old:RPool:first"], false)).unwrap();
    assert_eq!(report.mode, CleanMode::Preview);
    assert_eq!((report.new.units, report.new.objects), (2, 4));
    assert!(fake.records.lock().unwrap().is_empty() && fake.deleted().is_empty());
    // Kept groups, unknown groups and missing acknowledgements are refused.
    for (groups, expect) in [
        (vec!["in-use:RPool"], "is kept"),
        (vec!["nope"], "no group nope"),
        (vec![], "choose at least one group"),
    ] {
        let error = run(&fake, &request(&groups, true)).unwrap_err();
        assert!(format!("{error:#}").contains(expect), "{error:#}");
    }
    let tr = classify(&world())
        .group_of("archive:virtual-tr")
        .unwrap()
        .id
        .clone();
    let error = run(&fake, &request(&[&tr], true)).unwrap_err();
    assert!(format!("{error:#}").contains("--include-unconfirmed-generations"));
    let mut acked = request(&[&tr], false);
    acked.unconfirmed = true;
    assert_eq!(run(&fake, &acked).unwrap().new.units, 2);
    assert!(fake.records.lock().unwrap().is_empty());
}

#[test]
fn mark_grace_recheck_and_delete_archives_before_metadata() {
    let fake = Fake::new(world());
    let group = ["old:RPool:first"];
    let first = run(&fake, &request(&group, true)).unwrap();
    assert_eq!(first.mode, CleanMode::Applied);
    assert_eq!(first.new.units, 2);
    assert_eq!(first.next_deletion_unix, Some(NOW + 7 * DAY));
    assert!(fake.deleted().is_empty());
    // The view shows the pending marks with their due time.
    let v = view(&fake.observe().unwrap(), fake.records(), 7, None);
    let g = v.groups.iter().find(|g| g.id == group[0]).unwrap();
    assert_eq!(g.pending.marked, 2);
    assert_eq!(g.pending.last_due_unix, Some(NOW + 7 * DAY));
    // Before the grace period: waiting, nothing deleted.
    fake.advance(6 * DAY);
    let waiting = run(&fake, &request(&group, true)).unwrap();
    assert_eq!(waiting.waiting.units, 2);
    assert!(fake.deleted().is_empty());
    // After it: deleted, manifest first, archive before the metadata.
    fake.advance(2 * DAY);
    let done = run(&fake, &request(&group, true)).unwrap();
    assert_eq!(done.deleted.units, 2);
    let deleted = fake.deleted();
    assert_eq!(deleted.len(), 4);
    assert_eq!(deleted[0], format!("{ROOT}/virtual-old/manifest.json"));
    assert!(deleted.last().unwrap().contains("events-v6"));
    let s = scope("RPool");
    let state = fake.state();
    assert!(state["archive:virtual-old"].deleted);
    assert!(state[&format!("generation:{s}/first")].deleted);
    // Emptied folders are offered for removal, deepest first.
    let dirs = fake.removed_dirs.lock().unwrap().clone();
    let at = |d: &str| {
        dirs.iter()
            .position(|x| x == &format!("{ROOT}/{d}"))
            .unwrap()
    };
    assert!(at("virtual-old/data") < at("virtual-old"));
    // Nothing else was touched.
    assert!(deleted
        .iter()
        .all(|d| d.contains("virtual-old") || d.contains(&s)));
    assert!(classify(&fake.world.lock().unwrap())
        .units
        .contains_key("archive:virtual-both"));
}

#[test]
fn data_needed_again_is_released_and_never_deleted() {
    let fake = Fake::new(world());
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    // The current generation names the old archive again (e.g. a restore).
    rename(
        &mut fake.world.lock().unwrap().generations[0],
        &["virtual-live", "virtual-both", "virtual-old"],
    );
    fake.advance(8 * DAY);
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert_eq!(report.released, 1);
    assert!(fake.deleted().iter().all(|d| !d.contains("virtual-old")));
    assert!(!fake.state().contains_key("archive:virtual-old"));
    // The metadata no longer waits for the archive (it is in use now) and goes.
    assert_eq!(report.deleted.units, 1);
}

#[test]
fn recheck_before_deleting_releases_what_became_needed() {
    let fake = Fake::new(world());
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    fake.advance(8 * DAY);
    // Referenced again between the plan and the deletion: observe #0 plans,
    // so make the second observation of this run see the reference.
    struct Late<'a>(&'a Fake);
    impl StorageIo for Late<'_> {
        fn root(&self) -> String {
            self.0.root()
        }
        fn records(&self) -> Result<Vec<Record>> {
            self.0.records()
        }
        fn publish(&self, record: &Record) -> Result<()> {
            if record.step == Step::Deleting {
                self.0
                    .world
                    .lock()
                    .unwrap()
                    .live
                    .add_text("new upload", "virtual-old");
            }
            self.0.publish(record)
        }
        fn observe(&self) -> Result<Classified> {
            self.0.observe()
        }
        fn delete(&self, address: &str) -> Result<()> {
            self.0.delete(address)
        }
        fn remove_dir(&self, address: &str) {
            self.0.remove_dir(address)
        }
        fn now(&self) -> u64 {
            self.0.now()
        }
        fn new_id(&self) -> Result<String> {
            self.0.new_id()
        }
        fn worker(&self) -> String {
            self.0.worker()
        }
        fn grace_days(&self) -> u32 {
            7
        }
    }
    let report = run(&Late(&fake), &request(&["old:RPool:first"], true)).unwrap();
    // The archive's deletion was undone and its mark released; the old
    // metadata no longer waits for it (it is in use elsewhere) and goes.
    assert!(fake.deleted().iter().all(|d| !d.contains("virtual-old")));
    assert_eq!(report.deleted.units, 1);
    assert_eq!(report.released, 1);
    let state = fake.state();
    assert!(!state.contains_key("archive:virtual-old"));
    let s = scope("RPool");
    assert!(state[&format!("generation:{s}/first")].deleted);
}

#[test]
fn uncertain_postpones_and_an_interrupted_deletion_resumes() {
    let fake = Fake::new(world());
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    fake.advance(8 * DAY);
    // Uncertain at planning: postponed, nothing written.
    *fake.spoil_after.lock().unwrap() = Some(0);
    let before = fake.records.lock().unwrap().len();
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert_eq!(report.mode, CleanMode::Postponed);
    assert_eq!(fake.records.lock().unwrap().len(), before);
    // Uncertain at the re-check: this run's `Deleting` is undone.
    *fake.observed.lock().unwrap() = 0;
    *fake.spoil_after.lock().unwrap() = Some(1);
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert!(!report.postponed.is_empty());
    assert!(fake.deleted().is_empty());
    assert!(fake.state().values().all(|s| !s.deleting));
    // A provider error stops the deletion; the next run resumes it.
    *fake.spoil_after.lock().unwrap() = None;
    *fake.fail_delete.lock().unwrap() = Some(format!("{ROOT}/virtual-old/data/00000001.bin"));
    assert!(run(&fake, &request(&["old:RPool:first"], true)).is_err());
    assert!(fake.state()["archive:virtual-old"].deleting);
    *fake.fail_delete.lock().unwrap() = None;
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert_eq!(report.deleted.units, 2);
    assert!(fake.state().values().all(|s| s.deleted));
}

#[test]
fn a_failed_recheck_releases_this_runs_deletion() {
    let fake = Fake::new(world());
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    fake.advance(8 * DAY);
    // The plan observes (#0); the re-check after `Deleting` (#1) fails.
    *fake.observed.lock().unwrap() = 0;
    *fake.fail_observe_after.lock().unwrap() = Some(1);
    let error = run(&fake, &request(&["old:RPool:first"], true)).unwrap_err();
    assert!(format!("{error:#}").contains("listing timed out"));
    assert!(fake.deleted().is_empty());
    let steps: Vec<Step> = fake
        .records
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.step)
        .collect();
    assert!(steps.contains(&Step::Deleting), "{steps:?}");
    let state = fake.state();
    assert!(state.values().all(|s| !s.deleting), "{state:?}");
    // The marks stay due: the next run deletes.
    assert!(state["archive:virtual-old"].due().is_some());
    *fake.fail_observe_after.lock().unwrap() = None;
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert_eq!(report.deleted.units, 2);
}

#[test]
fn speed_test_leftovers_are_marked_and_deleted_an_hour_later() {
    let mut w = world();
    w.listing
        .insert(".rpool-speedtest/run/f1".into(), entry(1, NOW - 2 * DAY));
    let fake = Fake::new(w);
    let preview = run(&fake, &request(&["speed-test-leftovers"], false)).unwrap();
    assert_eq!((preview.new.units, preview.due.units), (1, 0));
    // The first confirmed run only marks, with a one-hour grace.
    let marked = run(&fake, &request(&["speed-test-leftovers"], true)).unwrap();
    assert_eq!((marked.new.units, marked.deleted.units), (1, 0));
    assert_eq!(marked.next_deletion_unix, Some(NOW + SPEED_TEST_GRACE));
    assert!(fake.deleted().is_empty());
    assert_eq!(
        fake.state()["speedtest:run"].due(),
        Some(NOW + SPEED_TEST_GRACE)
    );
    fake.advance(SPEED_TEST_GRACE - 1);
    let waiting = run(&fake, &request(&["speed-test-leftovers"], true)).unwrap();
    assert_eq!(waiting.waiting.units, 1);
    assert!(fake.deleted().is_empty());
    fake.advance(1);
    let report = run(&fake, &request(&["speed-test-leftovers"], true)).unwrap();
    assert_eq!(report.deleted.units, 1);
    assert_eq!(
        fake.deleted(),
        vec![format!("{ROOT}/.rpool-speedtest/run/f1")]
    );
    assert!(fake
        .removed_dirs
        .lock()
        .unwrap()
        .contains(&format!("{ROOT}/.rpool-speedtest")));
    // A group holding most of the provider trips the guard.
    let mut w = world();
    archive(&mut w, "virtual-big", 40);
    rename(
        &mut w.generations[1],
        &["virtual-old", "virtual-both", "virtual-big"],
    );
    let fake = Fake::new(w);
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    fake.advance(8 * DAY);
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    assert!(report.guard.is_some());
    assert!(fake.deleted().is_empty());
    let mut forced = request(&["old:RPool:first"], true);
    forced.force = true;
    let report = run(&fake, &forced).unwrap();
    assert_eq!(report.deleted.units, 3);
}

#[test]
fn cancel_drops_pending_marks() {
    let fake = Fake::new(world());
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    let report = run(
        &fake,
        &Request {
            cancel: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((report.mode, report.released), (CleanMode::Cancelled, 2));
    assert!(fake.state().is_empty());
}

#[test]
fn metadata_waits_for_archives_of_unselected_shared_groups() {
    let mut w = world();
    archive(&mut w, "virtual-shared", 1);
    rename(
        &mut w.generations[1],
        &["virtual-old", "virtual-both", "virtual-shared"],
    );
    rename(&mut w.generations[2], &["virtual-tr", "virtual-shared"]);
    let fake = Fake::new(w);
    run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    fake.advance(8 * DAY);
    let report = run(&fake, &request(&["old:RPool:first"], true)).unwrap();
    // The exclusive archive goes, the metadata waits for the shared one.
    assert_eq!(report.deleted.units, 1);
    assert!(report.notes.iter().any(|n| n.contains("wait")));
    assert!(fake.deleted().iter().all(|d| d.contains("virtual-old")));
}

#[test]
fn view_orders_kept_groups_first_and_reports_marks() {
    let c = classify(&world());
    let mark = Record::new(
        Step::Mark,
        "m",
        [(
            "archive:virtual-old".to_string(),
            records::Info {
                objects: 3,
                bytes: 21,
            },
        )]
        .into(),
        "pc",
        NOW,
    );
    let v = view(&c, Ok(vec![mark]), 7, Some(2));
    assert!(!v.groups[0].deletable);
    assert!(v.groups.last().unwrap().deletable);
    let old = v.groups.iter().find(|g| g.id == "old:RPool:first").unwrap();
    assert_eq!((old.pending.marked, old.pending.bytes), (1, 21));
    assert_eq!(v.undecryptable, Some(2));
    let text = crate::presentation::provider_storage::view_text(&v);
    assert!(text.contains("old:RPool:first") && text.contains("do not decrypt"));
    let broken = view(&c, Err(anyhow!("offline")), 7, None);
    assert!(broken.notes[0].contains("offline"));
}

#[test]
fn a_speed_test_written_during_the_grace_is_kept() {
    let mut w = world();
    w.listing
        .insert(".rpool-speedtest/run/f1".into(), entry(1, NOW - 2 * DAY));
    let fake = Fake::new(w);
    run(&fake, &request(&["speed-test-leftovers"], true)).unwrap();
    // A test resumed in the same folder: the re-check keeps it.
    fake.world
        .lock()
        .unwrap()
        .listing
        .insert(".rpool-speedtest/run/f2".into(), entry(1, NOW + 60));
    fake.advance(SPEED_TEST_GRACE);
    let error = run(&fake, &request(&["speed-test-leftovers"], true)).unwrap_err();
    assert!(format!("{error:#}").contains("no group speed-test-leftovers"));
    assert!(fake.deleted().is_empty());
}

#[test]
fn mark_only_marks_and_deletes_nothing() {
    let mut w = world();
    w.listing
        .insert(".rpool-speedtest/run/f1".into(), entry(1, NOW - 2 * DAY));
    let fake = Fake::new(w);
    let groups = ["old:RPool:first", "speed-test-leftovers"];
    run(&fake, &request(&groups, true)).unwrap();
    fake.advance(8 * DAY);
    // Every unit is due, plus a new one: marked, nothing deleted.
    fake.world
        .lock()
        .unwrap()
        .listing
        .insert(".rpool-speedtest/run2/f1".into(), entry(1, NOW - 2 * DAY));
    let mut mark = request(&groups, true);
    mark.mark_only = true;
    let report = run(&fake, &mark).unwrap();
    assert_eq!(report.mode, CleanMode::Applied);
    assert_eq!((report.new.units, report.due.units), (1, 3));
    assert_eq!(report.deleted.units, 0);
    assert!(fake.deleted().is_empty());
    assert!(report.notes.iter().any(|n| n.contains("not deleted")));
    let state = fake.state();
    assert!(state.contains_key("speedtest:run2"));
    assert!(state.values().all(|s| !s.deleting && !s.deleted));
    // A normal confirmed run deletes the due units.
    let report = run(&fake, &request(&groups, true)).unwrap();
    assert_eq!(report.deleted.units, 3);
}

#[test]
fn only_content_manifests_make_old_data_deletable() {
    let mut world = world();
    // A put archive with a manifest is never owned by drive metadata, not
    // even when an old generation's content manifest names it.
    archive(&mut world, "photo.jpg-0123456789abcdef", 1);
    rename(
        &mut world.generations[1],
        &["virtual-old", "virtual-both", "photo.jpg-0123456789abcdef"],
    );
    // A put archive that old events only mention (a path, a key).
    archive(&mut world, "put-0123456789abcdef", 1);
    world.generations[1].refs.add_json(
        "events",
        &serde_json::json!({"path": "docs/put-0123456789abcdef/readme", "put-0123456789abcdef": 1}),
    );
    // A drive archive an old generation only mentions in a file path.
    archive(&mut world, "virtual-mentioned", 1);
    world.generations[2]
        .refs
        .add_text("events", "notes/virtual-mentioned/a.txt");
    let c = classify(&world);
    for id in ["photo.jpg-0123456789abcdef", "put-0123456789abcdef"] {
        let g = group_of(&c, &format!("archive:{id}"));
        assert_eq!(
            (g.kind, g.deletable),
            (GroupKind::UnknownPutArchive, false),
            "{id}"
        );
    }
    let g = group_of(&c, "archive:virtual-mentioned");
    assert!(!g.deletable, "{g:?}");
    // What old content manifests do name stays deletable, shard folders included.
    assert!(group_of(&c, "archive:virtual-old").deletable);
    archive(&mut world, "virtual-borrowed", 1);
    let mut manifest = crate::migration::test_support::manifest("virtual-tr", 10, 4, 2, 1, &[ROOT]);
    manifest.shards[0].object = format!("{ROOT}/virtual-borrowed/data/00000000.bin");
    world.generations[2].owns = super::classify::owned_archives([&manifest]);
    let c = classify(&world);
    let g = group_of(&c, "archive:virtual-borrowed");
    assert_eq!((g.kind, g.deletable), (GroupKind::OldGeneration, true));
    assert_eq!(g.id, group_of(&c, "archive:virtual-tr").id);
}
