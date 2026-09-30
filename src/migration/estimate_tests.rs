use super::*;
use crate::migration::model::MissingReason;
use crate::migration::test_support::{manifest, policy};
use crate::storage::admin::budget::TargetBudget;

const MIB: u64 = 1048576;

#[test]
fn relocation_is_a_full_copy_with_rebuilt_shards_and_two_readbacks() {
    // 4 data shards in two 2+1 groups (6 shards of 1 MiB); parity on c:.
    let m = manifest("x", 4 * MIB, MIB, 2, 1, &["a:", "b:", "c:"]);
    let ok = vec![ShardState::Ok; m.shards.len()];
    let parity: BTreeSet<u32> = m
        .shards
        .iter()
        .filter(|s| s.remote == "c:")
        .map(|s| s.index)
        .collect();
    let t = relocate_transfer(&m, &ok, &parity).unwrap();
    assert_eq!(
        t.upload,
        6 * MIB,
        "every shard is written to the new archive"
    );
    assert_eq!(t.new_storage, 6 * MIB);
    assert_eq!(t.download, 6 * MIB + 12 * MIB, "copy + two readbacks");
    assert_eq!(t.specs.len(), 6);
    let mut gone = ok.clone();
    for &i in &parity {
        gone[i as usize] = ShardState::Missing(MissingReason::RemoteRemoved);
    }
    let t = relocate_transfer(&m, &gone, &parity).unwrap();
    assert_eq!(
        t.download,
        4 * MIB + 12 * MIB,
        "readable shards + readbacks"
    );
    assert_eq!(t.upload, 6 * MIB, "rebuilt parity is written too");
    let t = relocate_transfer(&m, &ok, &BTreeSet::new()).unwrap();
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
