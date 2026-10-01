use super::*;
use crate::migration::test_support::{manifest, set, FakeCloud};

const MIB: u64 = 1048576;

#[test]
fn lsjson_and_manifest_ids_skip_dirs_bookkeeping_and_nesting() {
    let files = parse_lsjson(
        br#"[{"Path":"a","Size":-1,"IsDir":true},{"Path":"a/manifest.json","Size":10,"IsDir":false},
            {"Path":".rpool-sync/x/manifest.json","Size":3},{"Path":"b/c/manifest.json","Size":3},
            {"Path":"virtual-1/manifest.json","Size":4}]"#,
    )
    .unwrap();
    assert_eq!(files.len(), 4);
    assert_eq!(
        manifest_ids(&files),
        vec![("a".into(), 10), ("virtual-1".into(), 4)]
    );
    assert!(is_drive_archive("virtual-1") && !is_drive_archive("peer-v7-x"));
    assert!(!is_drive_archive("reprocess-1"));
}

#[test]
fn finds_cloud_only_archives_skips_drive_and_reports_unreadable() {
    let mut cloud = FakeCloud::default();
    let a = manifest("a1", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    cloud.store(&a, &["a:", "b:", "c:"]);
    let drive = manifest("virtual-1", MIB, MIB, 1, 0, &["a:"]);
    cloud.store(&drive, &["a:"]);
    cloud
        .files
        .insert("b:broken/manifest.json".into(), b"{}".to_vec());
    if let RemoteListing::Listed(files) = cloud.listings.get_mut("b:").unwrap() {
        files.insert("broken/manifest.json".into(), 2);
    }
    let target = set(&["a:", "b:", "c:"]);
    let listings: BTreeMap<_, _> = target.iter().map(|r| (r.clone(), cloud.list(r))).collect();
    let out = enumerate(&cloud, &target, &InventoryStore::default(), &listings);
    assert_eq!(out.drive_skipped, 1);
    assert_eq!(out.found.len(), 2);
    let loaded: Vec<_> = out
        .found
        .iter()
        .filter_map(|f| match f {
            Found::Loaded(l) => Some(l.manifest.archive_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(loaded, vec!["a1"]);
    assert!(out.found.iter().any(|f| matches!(f,
        Found::Unreadable { archive_id, .. } if archive_id == "broken")));
    // Identical replicas (same listed size) are read once.
    let reads = cloud.reads.lock().unwrap();
    assert_eq!(reads.iter().filter(|r| r.contains("a1/")).count(), 1);
}

#[test]
fn conflicting_replicas_pick_the_newest_and_note_it() {
    let mut cloud = FakeCloud::default();
    let old = manifest("a1", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let mut new = old.clone();
    new.created_unix = 200;
    new.original_name = "renamed-longer-name.bin".into();
    cloud.store(&old, &["a:"]);
    cloud.store(&new, &["b:"]);
    let target = set(&["a:", "b:", "c:"]);
    let listings: BTreeMap<_, _> = target.iter().map(|r| (r.clone(), cloud.list(r))).collect();
    let out = enumerate(&cloud, &target, &InventoryStore::default(), &listings);
    let Found::Loaded(l) = &out.found[0] else {
        panic!("not loaded")
    };
    assert_eq!(l.manifest.created_unix, 200);
    assert!(l.source.starts_with("b:"));
    assert!(
        out.notes[0].contains("different manifests"),
        "{:?}",
        out.notes
    );
}

#[test]
fn inventory_entries_of_the_pool_use_their_local_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let a = manifest("a1", 2 * MIB, MIB, 2, 1, &["a:", "x:", "c:"]);
    let local = dir.path().join("a1.rpool.json");
    fs::write(&local, serde_json::to_vec_pretty(&a).unwrap()).unwrap();
    let other = manifest("o1", MIB, MIB, 1, 0, &["z:"]);
    let mut inventory = InventoryStore::default();
    for (m, source) in [
        (&a, local.to_string_lossy().to_string()),
        (&other, "/nope".into()),
    ] {
        inventory.entries.insert(
            m.archive_id.clone(),
            crate::inventory::entry_from_manifest(m, source),
        );
    }
    let mut cloud = FakeCloud::default();
    cloud.store(&a, &["a:"]);
    let target = set(&["a:", "c:"]);
    let listings: BTreeMap<_, _> = target.iter().map(|r| (r.clone(), cloud.list(r))).collect();
    let out = enumerate(&cloud, &target, &inventory, &listings);
    assert_eq!(out.found.len(), 1, "o1 is not on the pool");
    let Found::Loaded(l) = &out.found[0] else {
        panic!()
    };
    assert_eq!(l.source, local.to_string_lossy());
    assert_eq!(
        cloud.reads.lock().unwrap().len(),
        1,
        "same-size replica skipped"
    );
}
