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
    // Shard 0 sits on a removed remote and must move. The group is a=2, b=3;
    // the spread cap ceil(6/2)=3 keeps b at 3, so the forced shard evens it
    // out onto a (read-parallel spread) rather than piling a 4th onto b.
    let m = manifest(&["old", "a", "a", "b", "b", "b"], GIB);
    let (moves, _) = rb.plan(&m, &BTreeSet::from([0]));
    assert_eq!(moves.get(&0).map(String::as_str), Some("a:"));
    // Later archives see the quota the forced shard consumed on a.
    let budgets = rb.budgets.lock().unwrap();
    assert!(budgets["a"] < 3 * GIB);
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

#[test]
fn proportional_spreads_a_concentrated_group_and_is_idempotent() {
    // 6 shards piled on one account (5,1,0,0) across four equal accounts;
    // proportional rebalancing spreads them so every account holds some and
    // none keeps more than ceil(6/4)=2, for read parallelism.
    let target = pool(Placement::Proportional, &["a", "b", "c", "d"]);
    let quotas = [
        quota("a", 10 * GIB, 10 * GIB),
        quota("b", 10 * GIB, 10 * GIB),
        quota("c", 10 * GIB, 10 * GIB),
        quota("d", 10 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "a", "a", "a", "b"], GIB);
    let (moves, _) = rb.plan(&m, &BTreeSet::new());
    // Apply the moves to see the resulting layout.
    let placed: Vec<String> = (0..m.shards.len())
        .map(|i| {
            moves
                .get(&(i as u32))
                .cloned()
                .unwrap_or_else(|| m.shards[i].remote.clone())
        })
        .collect();
    let per = |who: &str| placed.iter().filter(|r| *r == who).count();
    for who in ["a:", "b:", "c:", "d:"] {
        assert!(per(who) >= 1, "every account used: {placed:?}");
        assert!(per(who) <= 2, "{who} over spread cap: {placed:?}");
    }
    // Idempotent: rebuilding a manifest from the new layout moves nothing more.
    let next: Vec<String> = placed
        .iter()
        .map(|s| s.trim_end_matches(':').to_owned())
        .collect();
    let next: Vec<&str> = next.iter().map(String::as_str).collect();
    let rb2 = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m2 = manifest(&next, GIB);
    assert!(
        rb2.plan(&m2, &BTreeSet::new()).0.is_empty(),
        "already spread"
    );
}
