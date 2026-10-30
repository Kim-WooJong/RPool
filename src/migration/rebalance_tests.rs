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
fn unbounded_free_ratio_rebalance_respects_only_explicit_account_caps() {
    let mut quotas = [quota("a", 9 * GIB, 10 * GIB), quota("b", 2 * GIB, 10 * GIB)];
    let m = manifest(&["a", "a", "a", "a", "a", "a"], GIB / 16);
    let uncapped = Rebalancer::new(
        &pool(Placement::FreeRatioUnbounded, &["a", "b"]),
        &quotas,
        &|_| None,
    )
    .unwrap();
    assert!(uncapped.plan(&m, &BTreeSet::new()).0.is_empty());

    let capped =
        Rebalancer::new(&pool(Placement::FreeRatio, &["a", "b"]), &quotas, &|_| None).unwrap();
    assert!(!capped.plan(&m, &BTreeSet::new()).0.is_empty());

    quotas[0].shard_cap = Some(1);
    let explicit = Rebalancer::new(
        &pool(Placement::FreeRatioUnbounded, &["a", "b"]),
        &quotas,
        &|_| None,
    )
    .unwrap();
    let (moves, _) = explicit.plan(&m, &BTreeSet::new());
    assert_eq!(count(&moves, "b:"), 5, "{moves:?}");
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
    // Data a,b,c,a and parity b,c: two shards and at most two data shards each.
    let m = manifest(&["a", "b", "c", "a", "b", "c"], GIB / 16);
    assert!(rb.plan(&m, &BTreeSet::new()).0.is_empty());
}

#[test]
fn balance_tries_another_destination_when_the_freest_one_would_overshoot() {
    let target = pool(Placement::FreeRatioUnbounded, &["a", "b", "c"]);
    let quotas = [
        quota("a", 400 * GIB, 1000 * GIB),
        quota("b", 9 * GIB, 10 * GIB),
        quota("c", 800 * GIB, 1000 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "a", "a", "a", "a"], 6 * GIB);
    let (moves, _) = rb.plan(&m, &BTreeSet::new());
    assert!(!moves.is_empty(), "a -> c remains feasible");
    assert!(moves.values().all(|remote| remote == "c:"), "{moves:?}");
}

#[test]
fn balance_tries_another_source_when_the_fullest_one_would_overshoot() {
    let target = pool(Placement::FreeRatioUnbounded, &["a", "b", "c"]);
    let quotas = [
        quota("a", 2 * GIB, 10 * GIB),
        quota("b", 400 * GIB, 1000 * GIB),
        quota("c", 800 * GIB, 1000 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "b", "b", "b", "b", "b"], 6 * GIB);
    let (moves, _) = rb.plan(&m, &BTreeSet::new());
    assert!(!moves.is_empty(), "b -> c remains feasible");
    assert!(!moves.contains_key(&0), "a cannot move without overshoot");
    assert!(moves.values().all(|remote| remote == "c:"), "{moves:?}");
}

/// Shard locations after applying `moves` to `m`.
fn placed(m: &Manifest, moves: &Moves) -> Vec<String> {
    m.shards
        .iter()
        .map(|s| {
            moves
                .get(&s.index)
                .cloned()
                .unwrap_or_else(|| s.remote.clone())
        })
        .collect()
}

/// Data shards per remote after the moves.
fn data_on(m: &Manifest, moves: &Moves, remote: &str) -> usize {
    placed(m, moves)
        .iter()
        .zip(&m.shards)
        .filter(|(r, s)| *r == remote && s.kind == ShardKind::Data)
        .count()
}

/// RS 9+3 like the user's pool: data slots 0..9, parity 9..12.
fn manifest_9_3(remotes: &[&str], size: u64) -> Manifest {
    let mut m = manifest(&remotes[..6], size);
    m.coding.as_mut().unwrap().data_shards = 9;
    m.coding.as_mut().unwrap().parity_shards = 3;
    m.shards = remotes
        .iter()
        .enumerate()
        .map(|(i, r)| Shard {
            index: i as u32,
            offset: 0,
            size,
            remote: format!("{r}:"),
            object: format!("{r}:a/{i}"),
            blake3: String::new(),
            kind: if i < 9 {
                ShardKind::Data
            } else {
                ShardKind::Parity
            },
            group: 0,
            slot: i as u16,
        })
        .collect();
    m
}

#[test]
fn full_accounts_swap_data_and_parity_so_reads_hit_every_account() {
    // 9+3 over three accounts at the spread limit 4/4/4, but c holds three of
    // the parity shards: reads fetch 4 data shards from a and b, 1 from c.
    // Swapping keeps every account at 4 shards and evens data out to 3/3/3.
    let mut target = pool(Placement::Proportional, &["a", "b", "c"]);
    target.data_shards = 9;
    target.parity_shards = 3;
    let quotas = [
        quota("a", 5 * GIB, 10 * GIB),
        quota("b", 5 * GIB, 10 * GIB),
        quota("c", 5 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let layout = ["a", "a", "a", "a", "b", "b", "b", "b", "c", "c", "c", "c"];
    let m = manifest_9_3(&layout, GIB / 16);
    let (moves, reasons) = rb.plan(&m, &BTreeSet::new());
    assert_eq!(moves.len(), 4, "two swaps: {moves:?}");
    for who in ["a:", "b:", "c:"] {
        assert_eq!(
            data_on(&m, &moves, who),
            3,
            "{who}: {:?}",
            placed(&m, &moves)
        );
        assert_eq!(placed(&m, &moves).iter().filter(|r| *r == who).count(), 4);
    }
    assert!(
        reasons.iter().any(|r| r.contains("read speed")),
        "{reasons:?}"
    );
    // Idempotent: the new layout needs nothing more.
    let next: Vec<String> = placed(&m, &moves)
        .iter()
        .map(|s| s.trim_end_matches(':').to_owned())
        .collect();
    let next: Vec<&str> = next.iter().map(String::as_str).collect();
    let rb2 = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    assert!(rb2
        .plan(&manifest_9_3(&next, GIB / 16), &BTreeSet::new())
        .0
        .is_empty());
    // The preview lists every account before and after.
    let summary = rb.summary().join("\n");
    assert!(
        summary.contains("c: data shards 1 -> 3, parity shards 3 -> 1"),
        "{summary}"
    );
    assert!(
        summary.contains("a: data shards 4 -> 3, parity shards 0 -> 1"),
        "{summary}"
    );
}

#[test]
fn free_ratio_balance_does_not_undo_the_data_spread() {
    // 4+2 over four accounts (spread limit 2 each); d was just added and is
    // empty. Data a,b,c,a and parity b,c: first a data shard leaves a for d
    // (read spread), then free-ratio balance sends parity, never data, to d.
    let target = pool(Placement::Proportional, &["a", "b", "c", "d"]);
    let quotas = [
        quota("a", GIB, 10 * GIB),
        quota("b", GIB, 10 * GIB),
        quota("c", GIB, 10 * GIB),
        quota("d", 10 * GIB, 10 * GIB),
    ];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "b", "c", "a", "b", "c"], GIB / 4);
    let (moves, reasons) = rb.plan(&m, &BTreeSet::new());
    let after = placed(&m, &moves);
    for who in ["a:", "b:", "c:", "d:"] {
        assert_eq!(data_on(&m, &moves, who), 1, "{who}: {after:?}");
    }
    assert_eq!(after.iter().filter(|r| *r == "d:").count(), 2, "{after:?}");
    assert!(
        reasons.iter().any(|r| r.contains("read speed")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.contains("more free space")),
        "{reasons:?}"
    );
}

#[test]
fn packing_placements_keep_their_data_layout() {
    let target = pool(Placement::CapacityFirst, &["a", "b"]);
    let quotas = [quota("a", 5 * GIB, 10 * GIB), quota("b", 5 * GIB, 10 * GIB)];
    let rb = Rebalancer::new(&target, &quotas, &|_| None).unwrap();
    let m = manifest(&["a", "a", "a", "a", "b", "b"], GIB / 16);
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
    // Two shards leave a for c; b held only parity, so one of its parity
    // shards then trades places with a data shard for the read spread.
    let after = placed(&m, &moves);
    for who in ["a:", "b:", "c:"] {
        assert_eq!(after.iter().filter(|r| *r == who).count(), 2, "{after:?}");
        assert!(data_on(&m, &moves, who) >= 1, "{who} has data: {after:?}");
    }
    assert_eq!(count(&moves, "c:"), 2, "{moves:?}");
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
