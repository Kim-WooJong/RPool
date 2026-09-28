//! Runtime eligibility, never a mutation of the saved Pool or old manifests.
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RemoteCatalog};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Excluded {
    pub remote: String,
    pub reason: String,
    pub temporary: bool,
}

/// One account budget, counted once even when several remotes alias it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AccountCapacity {
    pub domain: String,
    pub total: u64,
    pub free: u64,
    pub occupied: u64,
    pub declared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct CapacityStatus {
    pub eligible: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<AccountCapacity>,
    #[serde(default)]
    pub physical_total: u64,
    #[serde(default)]
    pub physical_free: u64,
    #[serde(default)]
    pub physical_occupied: u64,
    #[serde(default)]
    pub quota_complete: bool,
    #[serde(default)]
    pub nominal_logical_upper: u64,
    #[serde(default)]
    pub remaining_logical_upper: u64,
    #[serde(default)]
    pub pending_physical_reservation: u64,

    #[serde(default)]
    pub pool_sync_roots: Vec<String>,
    #[serde(default)]
    pub desired_history_limit: usize,
    #[serde(default)]
    pub conflicts: Vec<super::peer_projection::Conflict>,
    pub excluded: Vec<Excluded>,
    pub logical_used: u64,
    #[serde(default)]
    pub committed_logical_used: Option<u64>,
    #[serde(default)]
    pub usage_scope: String,
    pub additional_estimate: u64,
    #[serde(default)]
    pub estimate_limited: bool,
    pub logical_ceiling_estimate: u64,
    pub affected_active: usize,
    pub retained_archives: usize,
    pub observed_unix: u64,
    pub note: String,
    #[serde(default)]
    pub spool_bytes: u64,
    #[serde(default)]
    pub spool_limit_bytes: u64,
    #[serde(default)]
    pub pending_writes: usize,
    #[serde(skip)]
    pub pending_ids: Vec<String>,
    #[serde(skip)]
    pub namespace_event_ids: Vec<String>,
    #[serde(default)]
    pub targets: Vec<crate::storage::admin::budget::TargetBudget>,
    #[serde(skip)]
    pub(super) budget: u64,
    #[serde(skip)]
    pub(super) known_archive_targets: BTreeSet<String>,
}

impl CapacityStatus {
    pub(crate) fn inspect(admin: &dyn BackendAdmin, policy: &PoolDefinition) -> Result<Self> {
        crate::pool::validate_pool(policy)?;
        let catalog = admin.catalog()?;
        Self::with_catalog(admin, &catalog, policy)
    }

    fn with_catalog(
        admin: &dyn BackendAdmin,
        catalog: &RemoteCatalog,
        policy: &PoolDefinition,
    ) -> Result<Self> {
        let mut status = Self::default();
        // Timestamp the start of observation, not the end of a slow multi-account query.
        status.observed_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let remotes = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
        let snapshot =
            crate::storage::admin::budget::BudgetSnapshot::query(admin, catalog, &remotes);
        status.eligible = snapshot.targets.iter().map(|t| t.remote.clone()).collect();
        status.excluded = snapshot
            .rejected
            .iter()
            .map(|(remote, reason, temporary)| Excluded {
                remote: remote.clone(),
                reason: reason.clone(),
                temporary: *temporary,
            })
            .collect();
        status.budget = snapshot.total_free()?;
        status.targets = snapshot.targets;
        status.update_account_totals(policy)?;
        status.recalculate(policy)?;
        let feasible = status.additional_estimate > 0;
        status.note = "Planner-verified single-file capacity estimate, not a fixed nominal disk size. Explicitly declared independent account budgets are summed; aliases and unverified accounts share a minimum quota within their group. Small files, metadata/encryption and external changes reduce usable space. OS replica-mode space remains local disk space.".into();
        if status.estimate_limited {
            status.note.push_str(" Simulation limit reached: displayed space is a verified lower bound, not the maximum.");
        }
        if status.targets.iter().any(|t| !t.declared) {
            status.note.insert_str(0, "Account identities are unverified: declare capacity groups below to combine independent accounts. ");
        }
        if !feasible {
            status.note.insert_str(
                0,
                "Capacity search found no feasible nonempty file within current quotas/outage constraints. This is not proof that every possible file size is infeasible. ",
            );
        }
        Ok(status)
    }

    fn update_account_totals(&mut self, policy: &PoolDefinition) -> Result<()> {
        let mut accounts = BTreeMap::<String, AccountCapacity>::new();
        for target in &self.targets {
            accounts
                .entry(target.capacity_domain.clone())
                .and_modify(|a| {
                    a.total = a.total.min(target.total);
                    a.free = a.free.min(target.free);
                    a.declared &= target.declared;
                })
                .or_insert(AccountCapacity {
                    domain: target.capacity_domain.clone(),
                    total: target.total,
                    free: target.free,
                    occupied: 0,
                    declared: target.declared,
                });
        }
        self.accounts = accounts.into_values().collect();
        for account in &mut self.accounts {
            account.occupied = account.total.saturating_sub(account.free);
        }
        let sum = |f: fn(&AccountCapacity) -> u64| -> Result<u64> {
            self.accounts.iter().try_fold(0u64, |n, a| {
                n.checked_add(f(a)).context("account capacity overflow")
            })
        };
        self.physical_total = sum(|a| a.total)?;
        self.physical_free = sum(|a| a.free)?;
        self.physical_occupied = sum(|a| a.occupied)?;
        self.quota_complete = self.excluded.is_empty()
            && !self.accounts.is_empty()
            && self.accounts.iter().all(|a| a.declared);
        let ratio = |bytes: u64| -> u64 {
            if policy.parity_shards == 0 {
                bytes
            } else {
                ((bytes as u128 * policy.data_shards as u128)
                    / (policy.data_shards + policy.parity_shards) as u128) as u64
            }
        };
        self.nominal_logical_upper = ratio(self.physical_total);
        self.remaining_logical_upper = ratio(self.physical_free);
        Ok(())
    }

    pub(crate) fn recalculate(&mut self, policy: &PoolDefinition) -> Result<()> {
        let shard = policy
            .shard_mib
            .checked_mul(1048576)
            .context("shard size overflow")?;
        let k = if policy.parity_shards == 0 {
            1
        } else {
            policy.data_shards as u64
        };
        let m = policy.parity_shards as u64;
        let data_group = shard.checked_mul(k).context("data group overflow")?;
        let group = shard.checked_mul(k + m).context("group overflow")?;
        let upper = self.budget / group;
        let cap = 65536 / (k + m);
        let mut low = 0;
        let mut high = upper.min(cap);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if self.check_upload(policy, mid * data_group).is_ok() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        self.additional_estimate = low * data_group;
        self.estimate_limited = upper > cap && low == cap;
        // Test the partial final group too; less than one full group is not zero space.
        // This is a verified feasible value, not a global optimum of greedy placement.
        if low < cap {
            let base = self.additional_estimate;
            // Greedy diversity changes when the number of data shards changes.
            // Search those ranges separately rather than assuming global monotonicity.
            for data_count in (0..k).rev() {
                let start = base.saturating_add(data_count * shard).saturating_add(1);
                let end = base
                    .saturating_add((data_count + 1) * shard)
                    .min(self.budget);
                if start > end {
                    continue;
                }
                if self.check_upload(policy, end).is_ok() {
                    self.additional_estimate = end;
                    break;
                }
                if self.check_upload(policy, start).is_err() {
                    continue;
                }
                let mut lo = start;
                let mut hi = end;
                while lo < hi {
                    let mid = lo + (hi - lo).div_ceil(2);
                    if self.check_upload(policy, mid).is_ok() {
                        lo = mid;
                    } else {
                        hi = mid - 1;
                    }
                }
                self.additional_estimate = lo;
                break;
            }
        }
        self.logical_ceiling_estimate = self.logical_used.saturating_add(self.additional_estimate);
        Ok(())
    }

    /// Reserve queued uploads against their actual quota-group placement. No remote writes.
    pub(crate) fn reserve_pending(&mut self, policy: &PoolDefinition, sizes: &[u64]) -> Result<()> {
        if sizes.is_empty() {
            self.logical_ceiling_estimate =
                self.logical_used.saturating_add(self.additional_estimate);
            return Ok(());
        }
        let shard = policy
            .shard_mib
            .checked_mul(1048576)
            .context("shard overflow")?;
        for &size in sizes {
            let coding = (size > 0 && policy.parity_shards > 0).then(|| Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: policy.data_shards,
                parity_shards: policy.parity_shards,
                stripe_size: 1048576,
            });
            let specs = crate::planning::physical_specs(size, shard, coding.as_ref())?;
            let snapshot = crate::storage::admin::budget::BudgetSnapshot {
                targets: self.targets.clone(),
                rejected: vec![],
            };
            let assignments = match crate::placement::assign_with_budget(
                &snapshot,
                &specs,
                policy.placement,
                coding.as_ref().map(|c| c.parity_shards),
            ) {
                Ok(a) => a,
                Err(_) => {
                    self.additional_estimate = 0;
                    self.logical_ceiling_estimate = self.logical_used;
                    self.note.push_str(" Pending uploads exceed verified placement budget; additional space reported as zero.");
                    return Ok(());
                }
            };
            for (spec, index) in specs.iter().zip(assignments) {
                let domain = self.targets[index].capacity_domain.clone();
                for t in &mut self.targets {
                    if t.capacity_domain == domain {
                        t.free = t.free.saturating_sub(spec.size);
                    }
                }
            }
            self.pending_physical_reservation = self
                .pending_physical_reservation
                .checked_add(physical_bytes(policy, size)?)
                .context("pending reservation overflow")?;
        }
        self.budget = crate::storage::admin::budget::BudgetSnapshot {
            targets: self.targets.clone(),
            rejected: vec![],
        }
        .total_free()?;
        self.recalculate(policy)
    }

    pub(crate) fn check_upload(&self, policy: &PoolDefinition, size: u64) -> Result<()> {
        if self.eligible.is_empty() {
            bail!("No quota-known upload targets; local changes are retained");
        }
        let physical = physical_bytes(policy, size)?;
        if physical > self.budget {
            bail!("Insufficient conservative quota budget including full parity shards; local changes retained");
        }
        let shard = policy
            .shard_mib
            .checked_mul(1048576)
            .context("shard size overflow")?;
        let coding = (size > 0 && policy.parity_shards > 0).then(|| Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: policy.data_shards,
            parity_shards: policy.parity_shards,
            stripe_size: 1048576,
        });
        let specs = crate::planning::physical_specs(size, shard, coding.as_ref())?;
        crate::placement::assign_with_budget(
            &crate::storage::admin::budget::BudgetSnapshot {
                targets: self.targets.clone(),
                rejected: vec![],
            },
            &specs,
            policy.placement,
            coding.as_ref().map(|c| c.parity_shards),
        )?;
        Ok(())
    }
}

pub(crate) fn physical_bytes(policy: &PoolDefinition, size: u64) -> Result<u64> {
    if size == 0 || policy.parity_shards == 0 {
        return Ok(size);
    }
    let shard = policy
        .shard_mib
        .checked_mul(1048576)
        .context("shard size overflow")?;
    let groups = size
        .div_ceil(shard)
        .max(1)
        .div_ceil(policy.data_shards as u64);
    groups
        .checked_mul(policy.parity_shards as u64)
        .and_then(|n| n.checked_mul(shard))
        .and_then(|n| size.checked_add(n))
        .context("physical size overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Admin {
        report: QuotaReport,
    }
    impl BackendAdmin for Admin {
        fn catalog(&self) -> Result<RemoteCatalog> {
            RemoteCatalog::parse(&serde_json::json!({
                "a":{"type":"drive"}, "b":{"type":"drive"},
                "x":{"type":"crypt","remote":"a:"},
                "y":{"type":"alias","remote":"a:folder"},
                "z":{"type":"crypt","remote":"b:"},
                "unknown":{"type":"s3"}
            }))
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            if remote == "b:" {
                return crate::storage::admin::unavailable_quota(remote, "temporary".into());
            }
            self.report.clone()
        }
        fn discover(&self) -> Result<Vec<String>> {
            unreachable!()
        }
        fn probe(&self, _: &str) -> Result<()> {
            unreachable!()
        }
        fn ensure_encrypted(&self, _: &str) -> Result<()> {
            unreachable!()
        }
    }
    fn admin() -> Admin {
        Admin {
            report: QuotaReport {
                remote: "a:".into(),
                total: Some(100 * 1048576),
                free: Some(80 * 1048576),
                used: Some(20 * 1048576),
                trashed: None,
                other: None,
                used_percent: None,
                error: None,
            },
        }
    }
    fn policy() -> PoolDefinition {
        PoolDefinition {
            remotes: vec!["x:".into(), "y:".into(), "z:".into(), "unknown:".into()],
            shard_mib: 1,
            data_shards: 8,
            parity_shards: 2,
            ..Default::default()
        }
    }
    #[test]
    fn partial_search_crosses_free_ratio_diversity_boundary() {
        let mib = 1048576;
        let mut s = CapacityStatus {
            eligible: vec!["a:".into(), "b:".into()],
            budget: 2 * mib + 1,
            targets: [("a", 2 * mib), ("b", 1)]
                .into_iter()
                .map(|(name, free)| crate::storage::admin::budget::TargetBudget {
                    remote: format!("{name}:"),
                    backing: name.into(),
                    capacity_domain: name.into(),
                    failure_domain: Some(name.into()),
                    declared: true,
                    total: 3 * mib,
                    free,
                })
                .collect(),
            ..Default::default()
        };
        let p = PoolDefinition {
            shard_mib: 1,
            data_shards: 2,
            parity_shards: 1,
            placement: Placement::FreeRatio,
            ..policy()
        };
        assert!(s.check_upload(&p, mib).is_err());
        s.check_upload(&p, mib + 1).unwrap();
        s.recalculate(&p).unwrap();
        assert_eq!(s.additional_estimate, mib + 1);
    }
    #[test]
    fn partial_group_is_usable_and_accounts_are_not_double_counted() {
        let mut a = admin();
        a.report.free = Some(2 * 1048576 + 17);
        let s = CapacityStatus::inspect(&a, &policy()).unwrap();
        assert_eq!(s.accounts.len(), 1);
        assert_eq!(s.physical_total, 100 * 1048576);
        assert_eq!(s.physical_occupied + s.physical_free, s.physical_total);
        assert_eq!(s.additional_estimate, 17);
        s.check_upload(&policy(), 17).unwrap();
        assert!(s.check_upload(&policy(), 18).is_err());
        assert!(!s.quota_complete);
    }
    #[test]
    fn pending_upload_reserves_parity_and_preserves_observed_account_quota() {
        let p = policy();
        let mut s = CapacityStatus::inspect(&admin(), &p).unwrap();
        let observed = s.physical_free;
        let before = s.additional_estimate;
        s.reserve_pending(&p, &[1]).unwrap();
        assert_eq!(s.pending_physical_reservation, 2 * 1048576 + 1);
        assert_eq!(s.physical_free, observed);
        assert_eq!(s.budget, observed - s.pending_physical_reservation);
        assert!(s.additional_estimate < before);
        assert!(s.check_upload(&p, s.additional_estimate).is_ok());
        s.reserve_pending(&p, &[100 * 1048576]).unwrap();
        assert_eq!(s.additional_estimate, 0);
    }
    #[test]
    fn large_pool_upper_bound_is_not_truncated_by_simulation_limit() {
        let mut a = admin();
        a.report.total = Some(100000 * 1048576);
        a.report.free = a.report.total;
        a.report.used = Some(0);
        let mut p = policy();
        p.parity_shards = 0;
        let s = CapacityStatus::inspect(&a, &p).unwrap();
        assert_eq!(s.nominal_logical_upper, 100000 * 1048576);
        assert_eq!(s.additional_estimate, 65536 * 1048576);
        assert!(s.estimate_limited);
    }
    #[test]
    fn pool_query_leaves_namespace_usage_unknown() {
        let report = crate::pool::capacity::inspect(&admin(), &policy()).unwrap();
        assert_eq!(report.namespace_used, None);
        assert_eq!(report.capacity.usage_scope, "not-queried");
        assert_eq!(
            report.capacity.additional_estimate,
            CapacityStatus::inspect(&admin(), &policy())
                .unwrap()
                .additional_estimate
        );
    }
    #[test]
    fn dynamic_backend_quota_supported_and_failed_queries_excluded_without_double_counting() {
        let p = policy();
        let s = CapacityStatus::inspect(&admin(), &p).unwrap();
        assert_eq!(s.eligible, ["x:", "y:", "unknown:"]);
        assert_eq!(s.excluded.len(), 1);
        assert!(s.excluded.iter().any(|e| e.remote == "z:" && e.temporary));
        assert!(s.targets.iter().any(|t| t.remote == "unknown:"));
        assert_eq!(s.budget, 80 * 1048576);
        assert_eq!(s.additional_estimate, 64 * 1048576);
        assert_eq!(p.remotes.len(), 4);
    }
    #[test]
    fn temporary_failure_recovers_and_full_is_known_not_unknown() {
        let mut a = admin();
        let p = policy();
        a.report.error = Some("offline".into());
        assert!(CapacityStatus::inspect(&a, &p).unwrap().eligible.is_empty());
        a.report.error = None;
        a.report.free = Some(0);
        let s = CapacityStatus::inspect(&a, &p).unwrap();
        assert_eq!(s.eligible.len(), 3);
        assert_eq!(s.additional_estimate, 0);
        assert!(s.check_upload(&p, 1).is_err());
    }
    #[test]
    fn missing_and_contradictory_quota_fail_closed() {
        let mut a = admin();
        let p = policy();
        a.report.free = None;
        assert!(CapacityStatus::inspect(&a, &p).unwrap().eligible.is_empty());
        a.report.free = Some(u64::MAX);
        assert!(CapacityStatus::inspect(&a, &p).unwrap().eligible.is_empty());
    }
    #[test]
    fn small_files_charge_full_parity_and_overflow_rejected() {
        let p = policy();
        assert_eq!(physical_bytes(&p, 1).unwrap(), 2 * 1048576 + 1);
        assert_eq!(
            physical_bytes(&p, 8 * 1048576 + 1).unwrap(),
            12 * 1048576 + 1
        );
        assert_eq!(physical_bytes(&p, 0).unwrap(), 0);
        assert!(physical_bytes(&p, u64::MAX).is_err());
        let mut s = CapacityStatus::inspect(&admin(), &p).unwrap();
        s.budget = 2 * 1048576;
        assert!(s.check_upload(&p, 1).is_err());
    }
    #[test]
    fn skewed_quotas_follow_selected_policy_and_empty_mount_files_cost_no_parity() {
        let targets = [("a", 1), ("b", 99)]
            .into_iter()
            .map(|(name, n)| crate::storage::admin::budget::TargetBudget {
                remote: format!("{name}:"),
                backing: name.into(),
                capacity_domain: name.into(),
                failure_domain: Some(name.into()),
                declared: true,
                total: 100 * 1048576,
                free: n * 1048576,
            })
            .collect();
        let s = CapacityStatus {
            eligible: vec!["a:".into(), "b:".into()],
            targets,
            budget: 100 * 1048576,
            ..Default::default()
        };
        let mut p = policy();
        p.parity_shards = 0;
        p.placement = Placement::RoundRobin;
        assert!(s.check_upload(&p, 2 * 1048576).is_ok());
        assert!(s.check_upload(&p, 3 * 1048576).is_err());
        p.placement = Placement::FreeRatio;
        assert!(s.check_upload(&p, 100 * 1048576).is_ok());
        let mut full = s.clone();
        full.budget = 0;
        for t in &mut full.targets {
            t.free = 0;
        }
        p.parity_shards = 2;
        assert!(full.check_upload(&p, 0).is_ok());
        assert!(full.check_upload(&p, 1).is_err());
    }
    #[test]
    fn failure_domains_are_independent_of_quota_domains() {
        let mut s = CapacityStatus {
            eligible: vec!["a:".into(), "b:".into(), "c:".into()],
            budget: 30 * 1048576,
            ..Default::default()
        };
        s.targets = ["a", "b", "c"]
            .into_iter()
            .map(|name| crate::storage::admin::budget::TargetBudget {
                remote: format!("{name}:"),
                backing: name.into(),
                capacity_domain: name.into(),
                failure_domain: Some("same-provider".into()),
                declared: true,
                total: 10 * 1048576,
                free: 10 * 1048576,
            })
            .collect();
        let mut p = policy();
        p.data_shards = 2;
        p.parity_shards = 1;
        p.placement = Placement::Resilient;
        assert!(s.check_upload(&p, 2 * 1048576).is_err());
        for t in &mut s.targets {
            t.failure_domain = Some(t.backing.clone());
        }
        assert!(s.check_upload(&p, 2 * 1048576).is_ok());
    }
    #[test]
    fn filtered_resilient_never_downgrades_or_claims_capacity() {
        let mut p = policy();
        p.placement = Placement::Resilient;
        let s = CapacityStatus::inspect(&admin(), &p).unwrap();
        assert_eq!(s.additional_estimate, 0);
        assert_eq!(p.placement, Placement::Resilient);
    }
}
