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
    pub additional_estimate: u64,
    pub logical_ceiling_estimate: u64,
    pub affected_active: usize,
    pub retained_archives: usize,
    pub observed_unix: u64,
    pub note: String,
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
        let mut reports = BTreeMap::new();
        let mut domains = BTreeMap::<String, u64>::new();
        let remotes = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
        for remote in remotes {
            let binding = match catalog.capacity(&remote) {
                Ok(b) if b.domain.is_some() => b,
                _ => {
                    status.excluded.push(Excluded {
                        remote,
                        reason: "Quota accounting scope is unknown".into(),
                        temporary: false,
                    });
                    continue;
                }
            };
            let report = reports
                .entry(binding.target.clone())
                .or_insert_with(|| admin.quota(&binding.target));
            let valid = report.error.is_none()
                && matches!((report.total, report.free), (Some(t), Some(f)) if f <= t)
                && !matches!((report.total, report.used), (Some(t), Some(u)) if u > t)
                && !matches!((report.total, report.used, report.free), (Some(t), Some(u), Some(f)) if u.checked_add(f).is_none_or(|sum| sum > t));
            if !valid {
                status.excluded.push(Excluded {
                    remote,
                    reason: if report.error.is_some() {
                        "Quota query failed; excluded until a successful refresh"
                    } else {
                        "Quota is missing or inconsistent"
                    }
                    .into(),
                    temporary: report.error.is_some(),
                });
                continue;
            }
            domains.insert(
                binding.domain.unwrap().as_str().to_owned(),
                report.free.unwrap(),
            );
            status.eligible.push(remote);
        }
        // Account independence is unproven, even for distinct config sections.
        // A common minimum is intentionally conservative and matches free-ratio.
        status.budget = domains.values().copied().min().unwrap_or(0);
        let shard = policy
            .shard_mib
            .checked_mul(1048576)
            .context("shard size overflow")?;
        let k = policy.data_shards as u64;
        let m = policy.parity_shards as u64;
        let group = shard.checked_mul(k + m).context("group size overflow")?;
        let feasible = policy.placement != Placement::Resilient
            || (m > 0 && (k + m).div_ceil(domains.len().max(1) as u64) <= m);
        status.additional_estimate = if !feasible {
            0
        } else if m == 0 {
            status.budget
        } else {
            (status.budget / group)
                .saturating_mul(shard)
                .saturating_mul(k)
        };
        status.observed_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        status.note = "Conservative additional capacity for full coding groups; small files need proportionally more parity. Distinct accounts are not proven independent, so the smallest known free quota is used, not a sum. Encryption/metadata, external writers and local staging space are not reserved. OS drive space remains LOCAL disk space.".into();
        if !feasible {
            status.note.insert_str(
                0,
                "Insufficient eligible targets for the configured parity bound. ",
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
    let groups = size.div_ceil(shard).div_ceil(policy.data_shards as u64);
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
    fn unknown_and_failed_quota_excluded_without_policy_mutation_and_aliases_not_summed() {
        let p = policy();
        let s = CapacityStatus::inspect(&admin(), &p).unwrap();
        assert_eq!(s.eligible, ["x:", "y:"]);
        assert_eq!(s.excluded.len(), 2);
        assert!(s.excluded.iter().any(|e| e.remote == "z:" && e.temporary));
        assert!(s
            .excluded
            .iter()
            .any(|e| e.remote == "unknown:" && !e.temporary));
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
        assert_eq!(s.eligible.len(), 2);
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
    fn filtered_resilient_never_downgrades_or_claims_capacity() {
        let mut p = policy();
        p.placement = Placement::Resilient;
        let s = CapacityStatus::inspect(&admin(), &p).unwrap();
        assert_eq!(s.additional_estimate, 0);
        assert_eq!(p.placement, Placement::Resilient);
    }
}
