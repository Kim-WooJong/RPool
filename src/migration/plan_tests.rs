use super::*;
use crate::migration::model::MissingReason;
use crate::migration::test_support::{manifest, policy, FakeCloud};

const MIB: u64 = 1048576;
const ALL: [&str; 4] = ["a:", "b:", "c:", "d:"];

/// Three RS 2+1 files spread over four remotes: f1 on a,b,c; f2 on b,c,d;
/// f3 on c,d,a.
fn fixture() -> (FakeCloud, Vec<Manifest>) {
    let mut cloud = FakeCloud::default();
    let files = vec![
        manifest("f1", 2 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]),
        manifest("f2", 2 * MIB, MIB, 2, 1, &["b:", "c:", "d:"]),
        manifest("f3", 2 * MIB, MIB, 2, 1, &["c:", "d:", "a:"]),
    ];
    for f in &files {
        cloud.store(f, &ALL);
    }
    (cloud, files)
}

fn run(cloud: &FakeCloud, remotes: &[&str], k: usize, m: usize) -> Plan {
    plan_with(
        cloud,
        "main",
        policy(remotes, MIB, k, m),
        &InventoryStore::default(),
        &PlanOptions::default(),
    )
    .unwrap()
}

fn action(plan: &Plan, id: &str) -> Action {
    plan.entries
        .iter()
        .find(|e| e.archive_id == id)
        .unwrap()
        .action
}

#[test]
fn removing_one_remote_relocates_without_loss_and_lists_each_remote_once() {
    let (mut cloud, _) = fixture();
    cloud.wipe("d:");
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    assert_eq!(plan.entries.len(), 3);
    assert_eq!(action(&plan, "f1"), Action::Unaffected);
    assert_eq!(action(&plan, "f2"), Action::Relocate);
    assert_eq!(action(&plan, "f3"), Action::Relocate);
    assert_eq!(plan.counts.lost, 0);
    assert_eq!(plan.counts.relocate, 2);
    // f2 lost its parity, f3 a data shard: each is copied in full to the new
    // archive (the lost shard rebuilt from K = 2), then read back once. Copy
    // features are unknown here, so nothing counts as server-side.
    let f2 = plan.entries.iter().find(|e| e.archive_id == "f2").unwrap();
    assert_eq!(f2.upload_bytes, 3 * MIB);
    assert_eq!(f2.download_bytes, 2 * MIB + 3 * MIB);
    assert_eq!(f2.losses[0].missing[0].reason, MissingReason::Missing);
    assert_eq!(plan.new_storage_bytes, 6 * MIB);
    assert_eq!(plan.upload_bytes, 6 * MIB);
    assert_eq!(plan.migration_id.len(), 32);
    assert!(plan.estimated_seconds.is_none() || plan.download_mib_s.is_some());
    // The removed remote is still listed (it is referenced), once.
    let lists = cloud.lists.lock().unwrap();
    for remote in ALL {
        assert_eq!(lists.iter().filter(|r| *r == remote).count(), 1, "{remote}");
    }
}

#[test]
fn readable_removed_remote_is_copied_not_rebuilt() {
    let (cloud, _) = fixture();
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    let f2 = plan.entries.iter().find(|e| e.archive_id == "f2").unwrap();
    assert_eq!(f2.action, Action::Relocate);
    assert_eq!(
        f2.download_bytes,
        3 * MIB + 3 * MIB,
        "streamed full copy + one readback"
    );
    assert!(f2.losses.is_empty());
    assert!(!plan
        .notes
        .iter()
        .any(|n| n.contains("server-side by the provider")));
}

#[test]
fn server_side_copy_features_cut_the_estimate_and_add_a_note() {
    let (mut cloud, _) = fixture();
    for remote in ["a:", "b:", "c:"] {
        cloud.features.insert(
            remote.into(),
            CopyFeatures {
                server_side_copy: true,
                ciphertext_hash: true,
            },
        );
    }
    // d: leaves but stays readable: f2's b:/c: shards are copied server-side,
    // its d: parity is streamed to a pool remote and read back.
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    let f2 = plan.entries.iter().find(|e| e.archive_id == "f2").unwrap();
    assert_eq!((f2.download_bytes, f2.upload_bytes), (2 * MIB, MIB));
    let f3 = plan.entries.iter().find(|e| e.archive_id == "f3").unwrap();
    assert_eq!((f3.download_bytes, f3.upload_bytes), (2 * MIB, MIB));
    assert_eq!(plan.new_storage_bytes, 6 * MIB, "still a full copy");
    let note = plan
        .notes
        .iter()
        .find(|n| n.contains("server-side by the provider"))
        .unwrap();
    assert!(note.starts_with("4 shard(s)"), "{note}");
    assert!(note.contains("ciphertext hash"), "{note}");
}

#[test]
fn second_missing_remote_makes_exactly_the_expected_files_lost() {
    let (mut cloud, _) = fixture();
    cloud.wipe("d:");
    cloud.wipe("c:");
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    assert_eq!(
        action(&plan, "f1"),
        Action::Unaffected,
        "degraded, repairable"
    );
    assert_eq!(action(&plan, "f2"), Action::Lost);
    assert_eq!(action(&plan, "f3"), Action::Lost);
    let f3 = plan.entries.iter().find(|e| e.archive_id == "f3").unwrap();
    assert_eq!(f3.losses[0].available, 1);
    assert_eq!((f3.download_bytes, f3.upload_bytes), (0, 0));
    assert!(plan.notes.iter().any(|n| n.contains("unrecoverable")));
}

#[test]
fn adding_a_remote_moves_nothing_and_coding_change_reencodes() {
    let (cloud, _) = fixture();
    let plan = run(&cloud, &["a:", "b:", "c:", "d:", "e:"], 2, 1);
    assert_eq!(plan.counts.unaffected, 3);
    assert_eq!((plan.download_bytes, plan.upload_bytes), (0, 0));
    let plan = run(&cloud, &ALL, 3, 1);
    assert_eq!(plan.counts.reencode, 3);
    // 2 MiB -> one 3+1 group: 2 MiB data + 1 MiB parity.
    assert_eq!(plan.new_storage_bytes, 3 * 3 * MIB);
}

#[test]
fn provider_error_is_unknown_never_lost() {
    let (mut cloud, _) = fixture();
    cloud.wipe("d:");
    cloud.listings.insert(
        "c:".into(),
        RemoteListing::Failed("rclone timed out".into()),
    );
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    assert_eq!(action(&plan, "f1"), Action::Unaffected, "a,b hold K shards");
    assert_eq!(action(&plan, "f2"), Action::Unknown);
    assert_eq!(action(&plan, "f3"), Action::Unknown);
    assert_eq!(plan.counts.lost, 0);
    assert!(plan.notes.iter().any(|n| n.contains("could not be listed")));
}

#[test]
fn unconfigured_removed_remote_is_not_called_and_counts_as_removed() {
    let (cloud, _) = fixture();
    let cloud = FakeCloud {
        configured: Some(["a", "b", "c"].iter().map(|s| s.to_string()).collect()),
        ..cloud
    };
    let plan = run(&cloud, &["a:", "b:", "c:"], 2, 1);
    let f2 = plan.entries.iter().find(|e| e.archive_id == "f2").unwrap();
    assert_eq!(f2.action, Action::Relocate);
    assert_eq!(f2.losses[0].missing[0].reason, MissingReason::RemoteRemoved);
    assert!(!cloud.lists.lock().unwrap().iter().any(|r| r == "d:"));
}

#[test]
fn full_probe_replaces_listing_for_affected_archives() {
    let (mut cloud, files) = fixture();
    let f2 = &files[1];
    let probes: Vec<(Shard, Probe)> = f2
        .shards
        .iter()
        .map(|s| {
            let p = if s.remote == "b:" {
                Probe::Corrupt {
                    found: "x".into(),
                    expected: "y".into(),
                }
            } else if s.remote == "d:" {
                Probe::Missing
            } else {
                Probe::Ok
            };
            (s.clone(), p)
        })
        .collect();
    cloud.full.insert("f2".into(), probes);
    let plan = plan_with(
        &cloud,
        "main",
        policy(&["a:", "b:", "c:"], MIB, 2, 1),
        &InventoryStore::default(),
        &PlanOptions {
            probe_full: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(action(&plan, "f2"), Action::Lost);
    // f3 has no full probe result: its quick result is kept, with a note.
    let f3 = plan.entries.iter().find(|e| e.archive_id == "f3").unwrap();
    assert_eq!(f3.action, Action::Relocate);
    assert!(f3.detail.as_deref().unwrap().contains("full probe failed"));
}

#[test]
fn rejects_invalid_speeds_and_keeps_quota_and_speed_inputs() {
    let (mut cloud, _) = fixture();
    cloud.quota = Some(false);
    let target = policy(&["a:", "b:", "c:"], MIB, 2, 1);
    let options = PlanOptions {
        download_mib_s: Some(10.0),
        upload_mib_s: Some(5.0),
        ..Default::default()
    };
    let plan = plan_with(
        &cloud,
        "main",
        target.clone(),
        &InventoryStore::default(),
        &options,
    )
    .unwrap();
    assert_eq!(plan.quota_ok, Some(false));
    assert_eq!(plan.upload_mib_s, Some(5.0));
    assert_eq!(plan.pool, "main");
    let bad = PlanOptions {
        upload_mib_s: Some(0.0),
        ..Default::default()
    };
    assert!(plan_with(&cloud, "main", target, &InventoryStore::default(), &bad).is_err());
}

/// Real rclone planning for scripts/linux-docker/pool-migrate-plan-e2e.sh:
/// plans pool `$RPOOL_E2E_POOL` and writes the plan JSON to `$RPOOL_E2E_OUT`.
#[test]
#[ignore = "needs rclone and a prepared config (Docker e2e)"]
fn e2e_plan_from_env() {
    let pool = std::env::var("RPOOL_E2E_POOL").expect("RPOOL_E2E_POOL");
    let out = std::env::var("RPOOL_E2E_OUT").expect("RPOOL_E2E_OUT");
    let options = PlanOptions {
        probe_full: std::env::var("RPOOL_E2E_FULL").is_ok(),
        workers: 2,
        ..Default::default()
    };
    let plan = plan("rclone", &pool, &options).unwrap();
    fs::write(out, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
}

#[test]
fn originals_replaced_by_an_earlier_migration_are_skipped() {
    let (mut cloud, _) = fixture();
    cloud.wipe("d:");
    let target = policy(&["a:", "b:", "c:"], MIB, 2, 1);
    let replaced: BTreeSet<String> = ["f2".to_string()].into();
    let plan = plan_with_replaced(
        &cloud,
        "main",
        target,
        &InventoryStore::default(),
        &PlanOptions::default(),
        &replaced,
    )
    .unwrap();
    assert!(plan.entries.iter().all(|e| e.archive_id != "f2"));
    assert!(plan.notes.iter().any(|n| n.contains("already replaced")));
}
