use super::*;
use crate::migration::model::MissingReason;
use crate::migration::test_support::{manifest, policy};
use crate::storage::admin::budget::TargetBudget;

const MIB: u64 = 1048576;

fn unknown(_: &str) -> Option<CopyFeatures> {
    None
}

#[test]
fn relocation_without_copy_features_streams_every_shard() {
    // 4 data shards in two 2+1 groups (6 shards of 1 MiB); parity on c:.
    let m = manifest("x", 4 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let ok = vec![ShardState::Ok; m.shards.len()];
    let parity: BTreeSet<u32> = m
        .shards
        .iter()
        .filter(|s| s.remote == "c:")
        .map(|s| s.index)
        .collect();
    let t = relocate_transfer(&m, &ok, &parity, &unknown).unwrap();
    assert_eq!(
        t.upload,
        6 * MIB,
        "every shard is written to the new archive"
    );
    assert_eq!(t.new_storage, 6 * MIB);
    assert_eq!(
        t.download,
        6 * MIB + 6 * MIB,
        "streamed copy + one readback"
    );
    assert_eq!(t.specs.len(), 6);
    let mut gone = ok.clone();
    for &i in &parity {
        gone[i as usize] = ShardState::Missing(MissingReason::RemoteRemoved);
    }
    let t = relocate_transfer(&m, &gone, &parity, &unknown).unwrap();
    // Each 2+1 group downloads K=2 shards; everything is uploaded, read back.
    assert_eq!(t.download, 4 * MIB + 6 * MIB, "K shards + readbacks");
    assert_eq!(t.upload, 6 * MIB, "rebuilt parity is written too");
    let t = relocate_transfer(&m, &ok, &BTreeSet::new(), &unknown).unwrap();
    assert_eq!((t.download, t.upload), (0, 0));
}

#[test]
fn reencode_uses_the_reprocess_formula() {
    let m = manifest("x", 3 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let target = policy(&["a:"], MIB, 3, 1);
    let t = reencode_transfer(&m, &target).unwrap();
    let new = 3 * MIB + MIB;
    assert_eq!(t.new_storage, new);
    assert_eq!((t.download, t.upload), (3 * MIB + 2 * new, new));
    assert_eq!(t.specs.iter().map(|s| s.size).sum::<u64>(), new);
}

fn snapshot(free: &[u64]) -> BudgetSnapshot {
    BudgetSnapshot {
        targets: free
            .iter()
            .enumerate()
            .map(|(i, &free)| TargetBudget {
                remote: format!("r{i}:"),
                backing: format!("r{i}"),
                capacity_domain: format!("q{i}"),
                failure_domain: Some(format!("d{i}")),
                declared: true,
                total: 1000,
                free,
            })
            .collect(),
        rejected: vec![],
        observed_targets: vec![],
    }
}

#[test]
fn quota_charges_archives_cumulatively() {
    let spec = |size| PhysicalSpec { group: 0, size };
    let one = vec![spec(10), spec(10), spec(10)];
    let s = snapshot(&[15, 15, 15]);
    assert!(fits(
        &s,
        Placement::Resilient,
        Some(1),
        std::slice::from_ref(&one)
    ));
    assert!(!fits(
        &s,
        Placement::Resilient,
        Some(1),
        &[one.clone(), one.clone()]
    ));
    assert!(fits(&s, Placement::Resilient, Some(1), &[vec![], one]));
}

#[test]
fn server_side_copies_cost_nothing_with_a_hash_and_a_readback_without() {
    // 4 data shards in two 2+1 groups; c: (parity) leaves the pool.
    let m = manifest("x", 4 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let ok = vec![ShardState::Ok; m.shards.len()];
    let moving: BTreeSet<u32> = m
        .shards
        .iter()
        .filter(|s| s.remote == "c:")
        .map(|s| s.index)
        .collect();
    let full = |_: &str| {
        Some(CopyFeatures {
            server_side_copy: true,
            ciphertext_hash: true,
        })
    };
    let t = relocate_transfer(&m, &ok, &moving, &full).unwrap();
    // Kept a:/b: shards: server-side. c: parity: streamed + readback.
    assert_eq!((t.server_side_shards, t.server_side_bytes), (4, 4 * MIB));
    assert_eq!((t.download, t.upload), (2 * MIB + 2 * MIB, 2 * MIB));
    assert_eq!(t.new_storage, 6 * MIB);
    assert_eq!(t.specs.len(), 6);

    let no_hash = |_: &str| {
        Some(CopyFeatures {
            server_side_copy: true,
            ciphertext_hash: false,
        })
    };
    let t = relocate_transfer(&m, &ok, &moving, &no_hash).unwrap();
    assert_eq!(t.server_side_readback_shards, 4);
    assert_eq!((t.download, t.upload), (4 * MIB + 4 * MIB, 2 * MIB));

    // Only a: copies server-side; b: is unknown, so it streams.
    let mixed = |r: &str| (r == "a:").then_some(full("a:")).flatten();
    let t = relocate_transfer(&m, &ok, &moving, &mixed).unwrap();
    assert_eq!(t.server_side_shards, 2);
    assert_eq!((t.download, t.upload), (8 * MIB, 4 * MIB));

    // c: gone: groups rebuild. K=2 readable shards are downloaded per group
    // for decoding; the kept ones are still copied server-side, the rebuilt
    // parity is uploaded and read back.
    let mut gone = ok.clone();
    for &i in &moving {
        gone[i as usize] = ShardState::Missing(MissingReason::RemoteRemoved);
    }
    let t = relocate_transfer(&m, &gone, &moving, &full).unwrap();
    assert_eq!(t.server_side_shards, 4);
    assert_eq!((t.download, t.upload), (4 * MIB + 2 * MIB, 2 * MIB));
}
