use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

pub(crate) fn plan_free_ratio(
    rclone: &str,
    remotes: &[String],
    specs: &[PhysicalSpec],
    prefer_group_diversity: bool,
) -> Result<Vec<usize>> {
    plan_free_ratio_with_admin(
        &RcloneAdmin::inherited(rclone),
        remotes,
        specs,
        prefer_group_diversity,
    )
}
pub(crate) fn plan_free_ratio_with_admin(
    admin: &dyn BackendAdmin,
    remotes: &[String],
    specs: &[PhysicalSpec],
    prefer_group_diversity: bool,
) -> Result<Vec<usize>> {
    let catalog = admin.catalog()?;
    #[derive(Clone)]
    struct Q {
        total: u64,
        free: u64,
        assigned: u64,
    }

    let mut backing_by_remote = Vec::with_capacity(remotes.len());
    let mut quotas: BTreeMap<String, Q> = BTreeMap::new();
    for remote in remotes {
        let binding = catalog.capacity(remote)?;
        let backing = binding
            .domain
            .as_ref()
            .map(|d| d.as_str().to_owned())
            .ok_or_else(|| anyhow!("capacity domain unresolved for free-ratio placement"))?;
        if !quotas.contains_key(&backing) {
            let report = admin.quota(&binding.target);
            if let Some(error) = report.error {
                bail!(
                    "backing remote does not provide usable quota data for --placement free-ratio: {} (storage target {}): {}",
                    backing,
                    remote,
                    error
                );
            }
            let total = report
                .total
                .ok_or_else(|| anyhow!("{backing}: rclone about did not return total"))?;
            let free = report
                .free
                .ok_or_else(|| anyhow!("{backing}: rclone about did not return free"))?;
            quotas.insert(
                backing.clone(),
                Q {
                    total,
                    free,
                    assigned: 0,
                },
            );
        }
        backing_by_remote.push(backing);
    }

    // Distinct configuration sections may still share one account. Without
    // explicit account identity, bound combined allocation by the smallest
    // available quota rather than summing possibly overlapping budgets.
    let shared_budget = quotas.values().map(|q| q.free).min().unwrap_or(0);
    let mut shared_assigned = 0u64;
    let mut result = Vec::with_capacity(specs.len());
    let mut group_counts: BTreeMap<u32, BTreeMap<String, usize>> = BTreeMap::new();

    for spec in specs {
        if spec.size > shared_budget.saturating_sub(shared_assigned) {
            bail!(
                "insufficient conservative shared quota budget; account independence is unverified"
            );
        }
        shared_assigned = shared_assigned
            .checked_add(spec.size)
            .ok_or_else(|| anyhow!("allocation overflow"))?;
        let counts = group_counts.entry(spec.group).or_default();

        let min_count = if prefer_group_diversity {
            remotes
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    let backing = &backing_by_remote[index];
                    let quota = quotas.get(backing)?;
                    if quota.free.saturating_sub(quota.assigned) < spec.size {
                        return None;
                    }
                    Some(*counts.get(backing).unwrap_or(&0))
                })
                .min()
        } else {
            None
        };

        let mut best: Option<(usize, f64)> = None;
        for (index, _) in remotes.iter().enumerate() {
            let backing = &backing_by_remote[index];
            let quota = quotas
                .get(backing)
                .ok_or_else(|| anyhow!("missing quota state for backing remote {backing}"))?;
            let available = quota.free.saturating_sub(quota.assigned);
            if available < spec.size {
                continue;
            }
            if let Some(min_count) = min_count {
                if *counts.get(backing).unwrap_or(&0) != min_count {
                    continue;
                }
            }
            let score = available as f64 / quota.total.max(1) as f64;
            if best.map(|(_, old)| score > old).unwrap_or(true) {
                best = Some((index, score));
            }
        }

        let index = best
            .map(|(index, _)| index)
            .ok_or_else(|| anyhow!("insufficient reported free space for planned shards"))?;
        let backing = backing_by_remote[index].clone();
        let quota = quotas
            .get_mut(&backing)
            .ok_or_else(|| anyhow!("missing quota state for backing remote {backing}"))?;
        quota.assigned = quota.assigned.saturating_add(spec.size);
        *counts.entry(backing).or_insert(0) += 1;
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
            assert!(plan_free_ratio_with_admin(&Admin, &remotes, &specs, false).is_err());
            assert!(plan_free_ratio_with_admin(&Admin, &remotes, &specs[..1], false).is_ok());
        }
    }
}
