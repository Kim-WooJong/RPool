use super::*;

const GIB: u64 = 1 << 30;

fn quota(name: &str, free: u64, total: u64) -> TargetBudget {
    TargetBudget {
        remote: format!("{name}:"),
        backing: name.into(),
        capacity_domain: name.into(),
        failure_domain: Some(name.into()),
        declared: true,
        total,
        free,
        shard_cap: None,
    }
}

fn pool(placement: Placement, remotes: &[&str]) -> PoolDefinition {
    PoolDefinition {
        remotes: remotes.iter().map(|r| format!("{r}:")).collect(),
        placement,
        data_shards: 4,
        parity_shards: 2,
        ..Default::default()
    }
}

/// One RS 4+2 group with shard `i` on `remotes[i]`.
fn manifest(remotes: &[&str], size: u64) -> Manifest {
    Manifest {
        version: 2,
        archive_id: "a".into(),
        original_name: "f".into(),
        original_size: 4 * size,
        shard_size: size,
        created_unix: 0,
        content_root_blake3: String::new(),
        coding: Some(Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 4,
            parity_shards: 2,
            stripe_size: 0,
        }),
        shards: remotes
            .iter()
            .enumerate()
            .map(|(i, r)| Shard {
                index: i as u32,
                offset: 0,
                size,
                remote: format!("{r}:"),
                object: format!("{r}:a/{i}"),
                blake3: String::new(),
                kind: if i < 4 {
                    ShardKind::Data
                } else {
                    ShardKind::Parity
                },
                group: 0,
                slot: i as u16,
            })
            .collect(),
    }
}

fn count(moves: &Moves, remote: &str) -> usize {
    moves.values().filter(|r| *r == remote).count()
}

#[test]
fn a_new_empty_account_takes_shards_up_to_the_parity_limit() {
    // Two full-ish accounts hold the whole group; "c" was just added.
    let target = pool(Placement::FreeRatio, &["a", "b", "c"]);
    let quotas = [
        quota("a", 2 * GIB, 10 * GIB),
        quota("b", 2 * GIB, 10 * GIB),
        quota("c", 10 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "a", "b", "b", "b"], GIB);
    let (moves, reasons) = rb.plan(&m, &BTreeSet::new());
    assert_eq!(count(&moves, "c:"), 2, "{moves:?}");
    assert!(!reasons.is_empty());
}

#[test]
fn a_balanced_pool_moves_nothing() {
    let target = pool(Placement::FreeRatio, &["a", "b", "c"]);
    let quotas = [
        quota("a", 5 * GIB, 10 * GIB),
        quota("b", 5 * GIB, 10 * GIB),
        quota("c", 5 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "b", "b", "c", "c"], GIB / 16);
    assert!(rb.plan(&m, &BTreeSet::new()).0.is_empty());
}

#[test]
fn over_cap_accounts_give_up_shards_even_without_a_free_space_gap() {
    // Round robin over three accounts: at most 2 shards of the group each.
    let target = pool(Placement::RoundRobin, &["a", "b", "c"]);
    let quotas = [
        quota("a", 5 * GIB, 10 * GIB),
        quota("b", 5 * GIB, 10 * GIB),
        quota("c", 5 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "a", "a", "b", "b"], GIB / 16);
    let (moves, _) = rb.plan(&m, &BTreeSet::new());
    assert_eq!(moves.len(), 2, "{moves:?}");
    assert_eq!(count(&moves, "c:"), 2);
}

#[test]
fn forced_shards_get_a_destination_and_later_archives_see_the_quota() {
    let target = pool(Placement::Proportional, &["a", "b"]);
    let quotas = [quota("a", 3 * GIB, 10 * GIB), quota("b", 8 * GIB, 10 * GIB)];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    // Shard 0 sits on a removed remote and must move.
    let m = manifest(&["old", "a", "a", "b", "b", "b"], GIB);
    let (moves, _) = rb.plan(&m, &BTreeSet::from([0]));
    assert_eq!(moves.get(&0).map(String::as_str), Some("b:"));
    // b has no longer any real lead over a.
    let budgets = rb.budgets.lock().unwrap();
    assert!(budgets["b"] < 8 * GIB);
}

#[test]
fn unknown_quota_or_outage_group_refuses_to_guess() {
    let target = pool(Placement::FreeRatio, &["a", "b"]);
    assert!(Rebalancer::new(&target, &[quota("a", 1, 2)], &|_| None).is_err());
    let target = pool(Placement::Resilient, &["a", "b"]);
    let quotas = [quota("a", 1, 2), quota("b", 1, 2)];
    assert!(Rebalancer::new(&target, &quotas, &|_| None).is_err());
    assert!(Rebalancer::new(&target, &quotas, &|r| Some(r.to_owned())).is_ok());
}
