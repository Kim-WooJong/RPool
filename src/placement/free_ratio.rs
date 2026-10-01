use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

pub(crate) fn plan_free_ratio(
    rclone: &str,
    remotes: &[String],
    specs: &[PhysicalSpec],
    cap: Option<usize>,
) -> Result<Vec<usize>> {
    plan_free_ratio_with_admin(&RcloneAdmin::inherited(rclone), remotes, specs, cap)
}
pub(crate) fn plan_free_ratio_with_admin(
    admin: &dyn BackendAdmin,
    remotes: &[String],
    specs: &[PhysicalSpec],
    cap: Option<usize>,
) -> Result<Vec<usize>> {
    let catalog = admin.catalog()?;
    let snapshot = crate::storage::admin::budget::BudgetSnapshot::query(admin, &catalog, remotes);
    if !snapshot.rejected.is_empty() {
        bail!("quota planning unavailable: {:?}", snapshot.rejected);
    }
    allocate(&snapshot, specs, cap)
}

/// Pick the target with the highest free ratio for every shard. With
/// `parity = Some(m)` one account holds at most `max(m, ceil(group/accounts))`
/// shards of a coding group, so losing one account stays within parity
/// whenever the account count makes that possible. `None` fills purely by
/// ratio (the proportional placement) with no outage bound.
///
/// Ratio-first is greedy, so when it strands a shard the same cap is retried
/// with the fewest-shards-first order, which spreads each group evenly.
pub(crate) fn allocate(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    parity: Option<usize>,
) -> Result<Vec<usize>> {
    let ratio_first = allocate_ordered(snapshot, specs, parity, false);
    match parity {
        Some(m) if m > 0 && ratio_first.is_err() => allocate_ordered(snapshot, specs, parity, true),
        _ => ratio_first,
    }
}

fn allocate_ordered(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    parity: Option<usize>,
    spread: bool,
) -> Result<Vec<usize>> {
    let mut budgets = snapshot.budgets();
    let targets = &snapshot.targets;
    let mut result = Vec::with_capacity(specs.len());
    let mut counts = BTreeMap::<u32, BTreeMap<String, usize>>::new();
    let distinct: BTreeSet<_> = targets.iter().map(|t| t.backing.as_str()).collect();
    let mut group_sizes = BTreeMap::<u32, usize>::new();
    for spec in specs {
        *group_sizes.entry(spec.group).or_default() += 1;
    }
    for spec in specs {
        let group = counts.entry(spec.group).or_default();
        let cap = parity
            .filter(|m| *m > 0)
            .map(|m| m.max(group_sizes[&spec.group].div_ceil(distinct.len().max(1))));
        let mut best: Option<(usize, usize, f64)> = None;
        for (i, t) in targets.iter().enumerate() {
            let free = budgets[&t.capacity_domain];
            let count = group.get(&t.backing).copied().unwrap_or(0);
            if free < spec.size || cap.is_some_and(|m| count >= m) {
                continue;
            }
            let rank = if spread { count } else { 0 };
            let ratio = free as f64 / t.total.max(1) as f64;
            if best.is_none_or(|(_, r, q)| rank < r || (rank == r && ratio > q)) {
                best = Some((i, rank, ratio));
            }
        }
        let index = best
            .context("insufficient domain quota for the requested placement")?
            .0;
        let t = &targets[index];
        *budgets.get_mut(&t.capacity_domain).unwrap() -= spec.size;
        *group.entry(t.backing.clone()).or_default() += 1;
        result.push(index);
    }
    Ok(result)
}

/// Place against the largest remaining independent account budget. This is
/// intentionally not an outage-safe placement: a coding group can put more
/// than M shards on one provider when that is where the capacity is.
pub(crate) fn allocate_capacity_first(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
) -> Result<Vec<usize>> {
    let mut budgets = snapshot.budgets();
    let mut result = Vec::with_capacity(specs.len());
    for spec in specs {
        let index = snapshot
            .targets
            .iter()
            .enumerate()
            .filter(|(_, target)| budgets[&target.capacity_domain] >= spec.size)
            .max_by(|(a, left), (b, right)| {
                budgets[&left.capacity_domain]
                    .cmp(&budgets[&right.capacity_domain])
                    .then_with(|| b.cmp(a))
            })
            .map(|(index, _)| index)
            .context("insufficient account quota for capacity-first placement")?;
        *budgets
            .get_mut(&snapshot.targets[index].capacity_domain)
            .unwrap() -= spec.size;
        result.push(index);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::admin::RemoteCatalog;
    struct Admin;
    impl BackendAdmin for Admin {
        fn catalog(&self) -> Result<RemoteCatalog> {
            RemoteCatalog::parse(
                &serde_json::json!({"a":{"type":"drive"},"alias":{"type":"crypt","remote":"a:folder"},"b":{"type":"drive"}}),
            )
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            QuotaReport {
                remote: remote.into(),
                total: Some(100),
                used: Some(0),
                free: Some(100),
                trashed: None,
                other: None,
                used_percent: Some(0.0),
                error: None,
            }
        }
        fn discover(&self) -> Result<Vec<String>> {
            panic!("unused")
        }
        fn probe(&self, _: &str) -> Result<()> {
            panic!("unused")
        }
        fn ensure_encrypted(&self, _: &str) -> Result<()> {
            panic!("unused")
        }
    }
    struct Uneven;
    impl BackendAdmin for Uneven {
        fn catalog(&self) -> Result<RemoteCatalog> {
            Admin.catalog()
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            let mut q = Admin.quota(remote);
            q.free = Some(if remote == "a:" { 30 } else { 300 });
            q.total = Some(300);
            q
        }
        fn discover(&self) -> Result<Vec<String>> {
            panic!("unused")
        }
        fn probe(&self, _: &str) -> Result<()> {
            panic!("unused")
        }
        fn ensure_encrypted(&self, _: &str) -> Result<()> {
            panic!("unused")
        }
    }
    #[test]
    fn free_ratio_needs_the_even_share_when_parity_cannot_cover_one_account() {
        let specs = vec![PhysicalSpec { group: 0, size: 1 }; 6];
        let assigned =
            plan_free_ratio_with_admin(&Uneven, &["a:".into(), "b:".into()], &specs, Some(2))
                .unwrap();
        assert_eq!(assigned.iter().filter(|i| **i == 0).count(), 3);
        assert_eq!(assigned.iter().filter(|i| **i == 1).count(), 3);
    }

    fn snapshot(free: &[u64]) -> crate::storage::admin::budget::BudgetSnapshot {
        use crate::storage::admin::budget::{BudgetSnapshot, TargetBudget};
        BudgetSnapshot {
            targets: free
                .iter()
                .enumerate()
                .map(|(i, f)| TargetBudget {
                    remote: format!("r{i}:"),
                    backing: format!("b{i}"),
                    capacity_domain: format!("d{i}"),
                    failure_domain: None,
                    declared: true,
                    free: *f,
                    total: 1_000,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn free_ratio_caps_each_account_at_parity_and_prefers_free_space() {
        // RS 9+3 over six accounts, one nearly full: it may get 0 shards, while
        // no account exceeds M = 3 shards of the group.
        let snap = snapshot(&[1_000, 900, 800, 700, 600, 20]);
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 12];
        let assigned = allocate(&snap, &specs, Some(3)).unwrap();
        let count = |t: usize| assigned.iter().filter(|i| **i == t).count();
        assert!((0..6).all(|t| count(t) <= 3), "{assigned:?}");
        assert_eq!(count(5), 0, "{assigned:?}");
    }

    #[test]
    fn proportional_fill_has_no_per_account_cap() {
        let snap = snapshot(&[1_000, 100]);
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 12];
        let assigned = allocate(&snap, &specs, None).unwrap();
        assert!(assigned.iter().filter(|i| **i == 0).count() > 3);
    }

    #[test]
    fn overlapping_or_unproven_accounts_never_double_available_budget() {
        for remotes in [
            vec!["a:".into(), "alias:".into()],
            vec!["a:".into(), "b:".into()],
        ] {
            let specs = vec![
                PhysicalSpec { group: 0, size: 60 },
                PhysicalSpec { group: 1, size: 60 },
            ];
            assert!(plan_free_ratio_with_admin(&Admin, &remotes, &specs, None).is_err());
            assert!(plan_free_ratio_with_admin(&Admin, &remotes, &specs[..1], None).is_ok());
        }
    }
}
