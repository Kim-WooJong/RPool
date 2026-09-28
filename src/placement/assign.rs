use crate::placement::plan_free_ratio;
use crate::prelude::*;

pub(crate) fn assign_remotes(
    rclone: &str,
    remotes: &[String],
    specs: &[PhysicalSpec],
    placement: Placement,
    parity_shards: Option<usize>,
) -> Result<Vec<usize>> {
    match placement {
        Placement::RoundRobin => balanced_assign(
            &remotes
                .iter()
                .map(|r| crate::storage::scheduler::remote_key(r))
                .collect::<Vec<_>>(),
            specs,
        ),
        Placement::FreeRatio => plan_free_ratio(rclone, remotes, specs, parity_shards.is_some()),
        Placement::Resilient => {
            use crate::storage::admin::{BackendAdmin, RcloneAdmin};
            let admin = RcloneAdmin::inherited(rclone);
            let catalog = admin.catalog()?;
            let snapshot =
                crate::storage::admin::budget::BudgetSnapshot::query(&admin, &catalog, remotes);
            if !snapshot.rejected.is_empty() {
                bail!(
                    "Resilient quota planning unavailable: {:?}",
                    snapshot.rejected
                );
            }
            let assigned = assign_with_budget(&snapshot, specs, placement, parity_shards)?;
            // Budget query may deduplicate/reorder targets. Never interpret its
            // indexes against the caller's original remote list.
            assigned
                .into_iter()
                .map(|i| {
                    remotes
                        .iter()
                        .position(|r| r == &snapshot.targets[i].remote)
                        .context("quota target missing from upload remotes")
                })
                .collect()
        }
    }
}

fn declared_failure(
    catalog: &crate::storage::admin::RemoteCatalog,
    remote: &str,
) -> Result<String> {
    catalog
        .capacity(remote)?
        .failure_domain
        .map(|d| d.as_str().to_owned())
        .context("Resilient placement requires a declared outage group for every backing remote")
}

pub(crate) fn assign_with_budget(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    placement: Placement,
    parity: Option<usize>,
) -> Result<Vec<usize>> {
    let assignments = match placement {
        Placement::FreeRatio => super::free_ratio::allocate(snapshot, specs, parity.is_some())?,
        Placement::RoundRobin => balanced_assign(
            &snapshot
                .targets
                .iter()
                .map(|t| crate::storage::scheduler::remote_key(&t.remote))
                .collect::<Vec<_>>(),
            specs,
        )?,
        Placement::Resilient => resilient_allocate(snapshot, specs, parity.unwrap_or(0))?,
    };
    let mut budgets = snapshot.budgets();
    for (spec, index) in specs.iter().zip(&assignments) {
        let free = budgets
            .get_mut(&snapshot.targets[*index].capacity_domain)
            .context("missing quota group")?;
        *free = free
            .checked_sub(spec.size)
            .context("Selected placement exceeds an account quota; local data retained")?;
    }
    Ok(assignments)
}

/// Quota-aware deterministic greedy placement, shared by admission and upload.
/// Largest shards first within each group avoid stranding full-size parity behind
/// a small final data shard. This is a feasible policy, not a global optimizer.
fn resilient_allocate(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    parity: usize,
) -> Result<Vec<usize>> {
    let targets = &snapshot.targets;
    let domains = targets
        .iter()
        .map(|t| {
            t.failure_domain
                .clone()
                .context("Declare outage groups before Resilient placement")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut budgets = snapshot.budgets();
    let mut counts = BTreeMap::<(u32, &str), usize>::new();
    let mut order: Vec<_> = (0..specs.len()).collect();
    order.sort_by_key(|&i| (specs[i].group, std::cmp::Reverse(specs[i].size), i));
    let mut result = vec![0; specs.len()];
    let mut cursor = 0;
    for i in order {
        let spec = &specs[i];
        let chosen = (0..targets.len())
            .map(|n| (cursor + n) % targets.len())
            .filter(|&j| {
                budgets[&targets[j].capacity_domain] >= spec.size
                    && (spec.size == 0 && parity == 0
                        || counts
                            .get(&(spec.group, domains[j].as_str()))
                            .copied()
                            .unwrap_or(0)
                            < parity)
            })
            .min_by_key(|&j| {
                (
                    counts
                        .get(&(spec.group, domains[j].as_str()))
                        .copied()
                        .unwrap_or(0),
                    std::cmp::Reverse(budgets[&targets[j].capacity_domain]),
                )
            })
            .context(
                "Resilient placement cannot fit quota and outage bounds; local data retained",
            )?;
        *budgets.get_mut(&targets[chosen].capacity_domain).unwrap() -= spec.size;
        *counts
            .entry((spec.group, domains[chosen].as_str()))
            .or_default() += 1;
        result[i] = chosen;
        cursor = (chosen + 1) % targets.len();
    }
    ensure_parity_bound(&domains, specs, &result, parity)?;
    Ok(result)
}

pub(crate) fn balanced_assign(targets: &[String], specs: &[PhysicalSpec]) -> Result<Vec<usize>> {
    if targets.is_empty() {
        bail!("placement requires a target");
    }
    let mut counts = BTreeMap::<u32, BTreeMap<&str, usize>>::new();
    let mut result = Vec::with_capacity(specs.len());
    let mut cursor = 0;
    for spec in specs {
        let group = counts.entry(spec.group).or_default();
        let index = (0..targets.len())
            .map(|n| (cursor + n) % targets.len())
            .min_by_key(|i| group.get(targets[*i].as_str()).copied().unwrap_or(0))
            .unwrap();
        *group.entry(&targets[index]).or_default() += 1;
        result.push(index);
        cursor = (index + 1) % targets.len();
    }
    Ok(result)
}

pub(crate) fn validate_resilient_plan(rclone: &str, plan: &UploadPlan) -> Result<()> {
    use crate::storage::admin::{BackendAdmin, RcloneAdmin};
    if plan.placement != Placement::Resilient {
        return Ok(());
    }
    let catalog = RcloneAdmin::inherited(rclone).catalog()?;
    validate_resilient_catalog(&catalog, plan)
}

fn validate_resilient_catalog(
    catalog: &crate::storage::admin::RemoteCatalog,
    plan: &UploadPlan,
) -> Result<()> {
    let targets = plan
        .shards
        .iter()
        .map(|s| declared_failure(catalog, &s.object))
        .collect::<Result<Vec<_>>>()?;
    let specs = plan
        .shards
        .iter()
        .map(|s| PhysicalSpec {
            group: s.group,
            size: s.size,
        })
        .collect::<Vec<_>>();
    ensure_parity_bound(
        &targets,
        &specs,
        &(0..targets.len()).collect::<Vec<_>>(),
        plan.coding.as_ref().map_or(0, |c| c.parity_shards),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(free: &[u64]) -> crate::storage::admin::budget::BudgetSnapshot {
        crate::storage::admin::budget::BudgetSnapshot {
            targets: free
                .iter()
                .enumerate()
                .map(|(i, &free)| crate::storage::admin::budget::TargetBudget {
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
        }
    }
    #[test]
    fn resilient_skips_full_targets_without_weakening_outage_bound() {
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 3];
        let assigned = assign_with_budget(
            &snapshot(&[0, 10, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1),
        )
        .unwrap();
        assert!(!assigned.contains(&0));
        assert_eq!(assigned.iter().collect::<BTreeSet<_>>().len(), 3);
        assert!(assign_with_budget(
            &snapshot(&[0, 0, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_err());
    }
    #[test]
    fn resilient_shares_account_budget_and_outage_counts_across_aliases() {
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 3];
        let mut s = snapshot(&[10, 10, 10]);
        s.targets[1].capacity_domain = s.targets[0].capacity_domain.clone();
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_err());
        let mut s = snapshot(&[20, 20, 20]);
        s.targets[1].failure_domain = s.targets[0].failure_domain.clone();
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_err());
    }
    #[test]
    fn resilient_budget_is_cumulative_and_partial_group_places_large_shards_first() {
        let specs: Vec<_> = (0..6)
            .map(|i| PhysicalSpec {
                group: i / 3,
                size: 10,
            })
            .collect();
        assert!(assign_with_budget(
            &snapshot(&[10, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_err());
        assert!(assign_with_budget(
            &snapshot(&[20, 20, 20]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_ok());
        let partial = [
            PhysicalSpec { group: 0, size: 1 },
            PhysicalSpec { group: 0, size: 10 },
        ];
        let assigned =
            assign_with_budget(&snapshot(&[10, 1]), &partial, Placement::Resilient, Some(1))
                .unwrap();
        assert_eq!(assigned, vec![1, 0]);
        assert!(assign_with_budget(
            &snapshot(&[0]),
            &[PhysicalSpec { group: 0, size: 0 }],
            Placement::Resilient,
            None
        )
        .is_ok());
    }

    #[test]
    fn resumed_strict_plan_revalidates_changed_backing_aliases() {
        let mut plan = crate::planning::build_upload_plan(
            "unused",
            8,
            4,
            "archive".into(),
            vec!["x:".into(), "y:".into(), "z:".into()],
            Placement::RoundRobin,
            Some(Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: 2,
                parity_shards: 1,
                stripe_size: 2,
            }),
        )
        .unwrap();
        plan.placement = Placement::Resilient;
        let catalog = |collapse| {
            let mut catalog = crate::storage::admin::RemoteCatalog::parse(&serde_json::json!({
                "a":{"type":"s3"}, "b":{"type":"s3"}, "c":{"type":"s3"},
                "x":{"type":"crypt","remote":"a:"},
                "y":{"type":"crypt","remote": if collapse {"a:"} else {"b:"}},
                "z":{"type":"crypt","remote":"c:"}
            }))
            .unwrap();
            catalog.set_domains(crate::storage::admin::domains::DomainStore {
                version: 1,
                remotes: ["a", "b", "c"]
                    .into_iter()
                    .map(|name| {
                        (
                            name.into(),
                            crate::storage::admin::domains::DomainIdentity {
                                capacity: name.into(),
                                failure: name.into(),
                            },
                        )
                    })
                    .collect(),
            });
            catalog
        };
        assert!(validate_resilient_catalog(&catalog(false), &plan).is_ok());
        assert!(validate_resilient_catalog(&catalog(true), &plan).is_err());
    }
    #[test]
    fn resilient_rejects_impossible_layout_and_collapsed_aliases() {
        assert!(ensure_parity_bound(
            &["a".into()],
            &[PhysicalSpec { group: 0, size: 0 }],
            &[0],
            0
        )
        .is_ok());
        let specs = vec![PhysicalSpec { group: 0, size: 1 }; 10];
        for (targets, valid) in [
            (vec!["a", "b", "c", "d", "e"], true),
            (vec!["a", "a", "b", "c", "d"], false),
            (vec!["a", "b", "c"], false),
        ] {
            let targets: Vec<String> = targets.into_iter().map(str::to_owned).collect();
            let assigned = balanced_assign(&targets, &specs).unwrap();
            assert_eq!(
                ensure_parity_bound(&targets, &specs, &assigned, 2).is_ok(),
                valid
            );
            assert!(ensure_parity_bound(&targets, &specs, &assigned, 0).is_err());
        }
    }
    #[test]
    fn repeated_paths_cannot_weight_one_target_and_final_groups_balance() {
        let targets = vec!["a".into(), "a".into(), "b".into(), "c".into()];
        let specs: Vec<_> = (0..13)
            .map(|i| PhysicalSpec {
                group: if i < 10 { 0 } else { 1 },
                size: 1,
            })
            .collect();
        let assignments = balanced_assign(&targets, &specs).unwrap();
        for group in [0, 1] {
            let mut counts = BTreeMap::new();
            for (spec, index) in specs.iter().zip(&assignments) {
                if spec.group == group {
                    *counts.entry(&targets[*index]).or_insert(0) += 1;
                }
            }
            assert_eq!(counts.len(), 3);
            assert!(counts.values().max().unwrap() - counts.values().min().unwrap() <= 1);
        }
    }
}

fn ensure_parity_bound(
    targets: &[String],
    specs: &[PhysicalSpec],
    assignments: &[usize],
    parity: usize,
) -> Result<()> {
    if parity == 0 && specs.iter().all(|s| s.size == 0) {
        return Ok(()); // Empty files have no data bytes to protect.
    }
    if parity == 0 {
        bail!("resilient placement requires parity_shards > 0");
    }
    let mut counts = BTreeMap::<(u32, &str), usize>::new();
    for (spec, index) in specs.iter().zip(assignments) {
        let count = counts.entry((spec.group, &targets[*index])).or_default();
        *count += 1;
        if *count > parity {
            bail!("resilient placement impossible: group {} exceeds {} shards on one resolved backing target; add independent targets or change K/M", spec.group, parity);
        }
    }
    Ok(())
}
