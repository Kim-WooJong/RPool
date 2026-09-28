//! Runtime eligibility, never a mutation of the saved Pool or old manifests.
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RemoteCatalog};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Excluded {
    pub remote: String,
    pub reason: String,
    pub temporary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct CapacityStatus {
    pub eligible: Vec<String>,
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
        let group = shard.checked_mul(k + m).context("group size overflow")?;
        let upper = status.budget / group;
        let cap = 65536 / (k + m);
        let mut low = 0;
        let mut high = upper.min(cap);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if status.check_upload(policy, mid * k * shard).is_ok() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        status.additional_estimate = low * k * shard;
        status.estimate_limited = upper > cap && low == cap;
        let feasible = status.check_upload(policy, k * shard).is_ok();
        status.observed_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        status.note = "Full-group logical capacity estimate, not a fixed nominal disk size. Explicitly declared independent account budgets are summed; aliases and unverified accounts share a minimum quota within their group. Small files, metadata/encryption and external changes reduce usable space. OS replica-mode space remains local disk space.".into();
        if status.estimate_limited {
            status.note.push_str(" Simulation limit reached: displayed space is a verified lower bound, not the maximum.");
        }
        if status.targets.iter().any(|t| !t.declared) {
            status.note.insert_str(0, "Account identities are unverified: declare capacity groups below to combine independent accounts. ");
        }
        if !feasible {
            status.note.insert_str(
                0,
                "Selected placement cannot fit a full group within current account quotas/outage constraints. ",
            );
        }
        Ok(status)
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
