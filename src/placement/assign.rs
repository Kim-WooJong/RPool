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
            let catalog = RcloneAdmin::inherited(rclone).catalog()?;
            let targets = remotes
                .iter()
                .map(|r| declared_failure(&catalog, r))
                .collect::<Result<Vec<_>>>()?;
            let assignments = balanced_assign(&targets, specs)?;
            ensure_parity_bound(&targets, specs, &assignments, parity_shards.unwrap_or(0))?;
            Ok(assignments)
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
        Placement::Resilient => {
            let domains = snapshot
                .targets
                .iter()
                .map(|t| {
                    t.failure_domain
                        .clone()
                        .context("Declare outage groups before Resilient placement")
                })
                .collect::<Result<Vec<_>>>()?;
            let assigned = balanced_assign(&domains, specs)?;
            ensure_parity_bound(&domains, specs, &assigned, parity.unwrap_or(0))?;
            assigned
        }
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
