use super::*;
use crate::migration::retire::test_fixture::{fixture, N1, O1};

fn reasons(s: &Selection) -> BTreeMap<String, KeepReason> {
    s.kept
        .iter()
        .map(|k| (k.archive_id.clone(), k.reason))
        .collect()
}

fn ids(s: &Selection) -> Vec<String> {
    s.candidates.iter().map(|i| i.archive_id.clone()).collect()
}

#[test]
fn selects_replaced_originals_and_orphans_only() {
    let f = fixture();
    let s = select(&f.plan, &f.records, &f.world, false);
    assert_eq!(ids(&s), vec!["old1", "old2", O1]);
    // 5 shards + 3 manifest replicas for old1, all inside old1/.
    let old1 = &s.candidates[0];
    assert_eq!(old1.objects.len(), 8);
    assert!(old1.objects.iter().all(|o| o.address.contains(":old1/")));
    assert_eq!(old1.replacement.as_deref(), Some(N1));
    assert_eq!(s.candidates[2].objects.len(), 1);
    // The lost archive and the replacements are never candidates.
    assert_eq!(reasons(&s).get("lost1"), Some(&KeepReason::Lost));
    assert!(!ids(&s)
        .iter()
        .any(|id| id.starts_with("migrate-a") || id == "keep"));
}

#[test]
fn anything_referenced_is_kept() {
    let mut f = fixture();
    // Another manifest borrows one object of old1.
    let mut borrower = crate::migration::test_support::manifest("borrower", 4, 4, 2, 1, &["a:"]);
    borrower.shards[0].object = "a:old1/data/00000000.bin".into();
    f.world
        .refs
        .add_manifest("inventory entry borrower", &borrower);
    // The drive names old2; another migration in progress names the orphan.
    f.world.refs.add_json(
        "drive v6 metadata",
        &serde_json::json!({"manifest": {"archive_id": "old2"}}),
    );
    f.world.refs.add_text("migration m2 (in progress)", O1);
    let s = select(&f.plan, &f.records, &f.world, false);
    assert!(s.candidates.is_empty());
    let r = reasons(&s);
    for id in ["old1", "old2", O1] {
        assert_eq!(r[id], KeepReason::Referenced, "{id}");
    }
    let old1 = s.kept.iter().find(|k| k.archive_id == "old1").unwrap();
    assert!(old1.detail.contains("inventory entry borrower"));
}

#[test]
fn uncertain_references_keep_everything() {
    let mut f = fixture();
    f.world
        .refs
        .uncertain("drive metadata: provider down".into());
    let s = select(&f.plan, &f.records, &f.world, false);
    assert!(s.candidates.is_empty());
    assert!(s
        .kept
        .iter()
        .filter(|k| k.reason != KeepReason::Lost)
        .all(|k| k.reason == KeepReason::ReferencesUncertain));
}

#[test]
fn replacement_must_reverify_and_original_must_be_unchanged() {
    let mut f = fixture();
    // A replacement shard disappeared.
    if let Some(RemoteListing::Listed(files)) = f.world.listings.get_mut("a:") {
        files.remove(&format!("{N1}/data/00000000.bin"));
    }
    // old2's manifest changed after the migration (e.g. a drain).
    if let Some(Ok(Some(m))) = f.world.originals.get_mut("old2") {
        m.created_unix += 1;
    }
    let s = select(&f.plan, &f.records, &f.world, false);
    let r = reasons(&s);
    assert_eq!(r["old1"], KeepReason::ReplacementUnverified);
    assert_eq!(r["old2"], KeepReason::OriginalChanged);
    assert_eq!(ids(&s), vec![O1]);
    // A failed full readback also blocks.
    let mut f = fixture();
    f.world
        .full_checks
        .insert(N1.into(), Err("hash mismatch".into()));
    let s = select(&f.plan, &f.records, &f.world, false);
    assert_eq!(reasons(&s)["old1"], KeepReason::ReplacementUnverified);
}

#[test]
fn unsafe_shapes_are_kept() {
    let mut f = fixture();
    // A shard outside the archive folder.
    if let Some(Ok(Some(m))) = f.world.originals.get_mut("old1") {
        m.shards[0].object = "a:elsewhere/x.bin".into();
        f.plan.entries[0].fingerprint = crate::manifest::manifest_fingerprint(m).unwrap();
    }
    // An unlistable account.
    let mut g = fixture();
    g.world
        .listings
        .insert("b:".into(), RemoteListing::Failed("timeout".into()));
    let s = select(&f.plan, &f.records, &f.world, false);
    assert_eq!(reasons(&s)["old1"], KeepReason::ObjectOutsideArchive);
    let s = select(&g.plan, &g.records, &g.world, false);
    assert!(s.candidates.is_empty());
    assert!(reasons(&s).values().any(|r| *r == KeepReason::Unreachable));
    // Drive revisions are never retired; ids not made by migrations are not orphans.
    assert!(!generated_id("virtual-abc"));
    assert!(generated_id(O1));
    assert!(!generated_id("migrate-ABCDEFABCDEFABCDEFABCDEF"));
}

#[test]
fn verified_copies_are_never_orphans() {
    let f = fixture();
    let orphans = orphan_ids(&f.records);
    assert_eq!(orphans.keys().collect::<Vec<_>>(), vec![O1]);
    let mut records = f.records.clone();
    let mut verified = records[3].clone();
    verified.state = RecordState::Verified;
    records.push(verified);
    assert!(orphan_ids(&records).is_empty());
}

#[test]
fn removed_accounts_are_opt_in() {
    let mut f = fixture();
    // old1 had a shard on an account that left the pool.
    let gone = "gone:".to_string();
    f.world.listings.insert(
        gone.clone(),
        RemoteListing::Listed(BTreeMap::from([("old1/data/x.bin".into(), 4)])),
    );
    if let Some(Ok(Some(m))) = f.world.originals.get_mut("old1") {
        m.shards[0].remote = gone.clone();
        m.shards[0].object = "gone:old1/data/x.bin".into();
        f.plan.entries[0].fingerprint = crate::manifest::manifest_fingerprint(m).unwrap();
    }
    let s = select(&f.plan, &f.records, &f.world, false);
    assert_eq!(s.left_on_removed.len(), 1);
    assert!(!s.candidates[0].objects.iter().any(|o| o.root == gone));
    let s = select(&f.plan, &f.records, &f.world, true);
    assert!(s.left_on_removed.is_empty());
    assert!(s.candidates[0].objects.iter().any(|o| o.root == gone));
}
