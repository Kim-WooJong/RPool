//! Runtime eligibility, never a mutation of the saved Pool or old manifests.
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RemoteCatalog};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A pool remote left out of upload placement, with the budget query's reason.
pub(crate) struct Excluded {
    /// Remote name as listed in the pool.
    pub remote: String,
    /// Human-readable reason it is not eligible.
    pub reason: String,
    /// Likely transient (e.g. quota query failed) rather than a permanent rejection.
    pub temporary: bool,
}

/// One account budget, counted once even when several remotes alias it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AccountCapacity {
    /// Capacity domain (shared-quota identity) of the account.
    pub domain: String,
    /// Total quota in bytes (minimum over aliasing remotes).
    pub total: u64,
    /// Free bytes (minimum over aliasing remotes).
    pub free: u64,
    /// `total - free` in bytes.
    pub occupied: u64,
    /// Every remote of this account has a declared capacity group.
    pub declared: bool,
}

/// A display-only scenario: each unverified backing section has its own quota.
/// It must never be used for upload admission or OS free-space reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IndependentQuotaScenario {
    /// Sum of totals with each unverified backing section counted separately (bytes).
    pub physical_total: u64,
    /// Sum of free bytes under the same assumption.
    pub physical_free: u64,
    /// `physical_total - physical_free`.
    pub physical_occupied: u64,
    /// `physical_total` scaled by the data/(data+parity) coding ratio.
    pub nominal_logical_upper: u64,
    /// `physical_free` scaled by the coding ratio.
    pub remaining_logical_upper: u64,
}

/// Physical shards the capacity search may simulate. Beyond this the estimate is a
/// verified lower bound (`estimate_limited`). 262,144 × 64 MiB ≈ 16 TiB keeps the
/// range the former 65,536 × 220 MiB limit covered; worst case ~2 s in release.
const MAX_SIMULATED_SHARDS: u64 = 262_144;

/// Scales raw bytes by `data / (data + parity)`; unchanged without parity.
fn coding_ratio(bytes: u64, policy: &PoolDefinition) -> u64 {
    if policy.parity_shards == 0 {
        bytes
    } else {
        ((bytes as u128 * policy.data_shards as u128)
            / (policy.data_shards + policy.parity_shards) as u128) as u64
    }
}

impl IndependentQuotaScenario {
    /// Builds the what-if scenario; `None` when every target is declared (nothing to show).
    fn from_targets(
        targets: &[crate::storage::admin::budget::TargetBudget],
        policy: &PoolDefinition,
    ) -> Result<Option<Self>> {
        if !targets.iter().any(|target| !target.declared) {
            return Ok(None);
        }
        let mut groups = BTreeMap::<(bool, String), (u64, u64)>::new();
        for target in targets {
            // Declared shared quotas stay shared. Unverified crypt/alias wrappers
            // resolve to one backing section, but separate sections may still
            // be the same account; this is only an explicit what-if scenario.
            let key = if target.declared {
                (true, target.capacity_domain.clone())
            } else {
                (false, target.backing.clone())
            };
            groups
                .entry(key)
                .and_modify(|(total, free)| {
                    *total = (*total).min(target.total);
                    *free = (*free).min(target.free);
                })
                .or_insert((target.total, target.free));
        }
        let (physical_total, physical_free) = groups.values().try_fold(
            (0u64, 0u64),
            |(total, free), (group_total, group_free)| {
                Ok::<_, anyhow::Error>((
                    total
                        .checked_add(*group_total)
                        .context("scenario total overflow")?,
                    free.checked_add(*group_free)
                        .context("scenario free overflow")?,
                ))
            },
        )?;
        Ok(Some(Self {
            physical_total,
            physical_free,
            physical_occupied: physical_total.saturating_sub(physical_free),
            nominal_logical_upper: coding_ratio(physical_total, policy),
            remaining_logical_upper: coding_ratio(physical_free, policy),
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Pool capacity snapshot for the mount status and GUI: per-account quotas, eligibility,
/// logical bounds, and a planner-verified estimate of how much more data fits.
/// Built by [`CapacityStatus::inspect`]; `virtual_drive::capacity` adds drive usage and reservations.
pub(crate) struct CapacityStatus {
    /// Remotes accepted for upload placement.
    pub eligible: Vec<String>,
    #[serde(default)]
    /// Per-account quotas, aliases counted once.
    pub accounts: Vec<AccountCapacity>,
    #[serde(default)]
    /// Sum of account totals in bytes.
    pub physical_total: u64,
    #[serde(default)]
    /// Sum of account free bytes.
    pub physical_free: u64,
    #[serde(default)]
    /// Sum of account occupied bytes.
    pub physical_occupied: u64,
    #[serde(default)]
    /// Display-only scenario for unverified accounts; `None` when all are declared.
    pub independent_quota_scenario: Option<IndependentQuotaScenario>,
    #[serde(default)]
    /// No excluded remotes and every account has a declared quota group.
    pub quota_complete: bool,
    #[serde(default)]
    /// `physical_total` scaled by the coding ratio (upper bound of logical size).
    pub nominal_logical_upper: u64,
    #[serde(default)]
    /// `physical_free` scaled by the coding ratio (upper bound of remaining logical space).
    pub remaining_logical_upper: u64,
    /// A tighter upper bound after accounting for a whole outage group's loss.
    /// None means the failure identities are not fully declared.
    #[serde(default)]
    pub resilient_remaining_upper: Option<u64>,
    #[serde(default)]
    /// Distinct outage/failure groups among eligible targets (resilient placement only).
    pub eligible_failure_groups: usize,
    #[serde(default)]
    /// Failure groups resilient placement needs: `ceil((data + parity) / parity)`; 0 otherwise.
    pub required_failure_groups: usize,
    #[serde(default)]
    /// Physical bytes reserved for pending and in-progress uploads.
    pub pending_physical_reservation: u64,

    #[serde(default)]
    /// Pool-sync roots of the drive; empty for a non-pool-sync drive.
    pub pool_sync_roots: Vec<String>,
    #[serde(default)]
    /// Concurrent-edit conflicts from the shared namespace (pool-sync only).
    pub conflicts: Vec<super::peer_projection::Conflict>,
    /// Remotes rejected from placement and why.
    pub excluded: Vec<Excluded>,
    /// Visible logical bytes of the drive (shared namespace plus local pending writes).
    pub logical_used: u64,
    #[serde(default)]
    /// Logical bytes of the committed shared namespace; `None` outside virtual mode.
    pub committed_logical_used: Option<u64>,
    #[serde(default)]
    /// Which usage `logical_used` counts, e.g. `"shared-namespace"` or `"not-queried"`.
    pub usage_scope: String,
    /// Verified additional logical bytes a single new file could use.
    pub additional_estimate: u64,
    #[serde(default)]
    /// The search hit `MAX_SIMULATED_SHARDS`; `additional_estimate` is only a lower bound.
    pub estimate_limited: bool,
    /// `logical_used + additional_estimate`.
    pub logical_ceiling_estimate: u64,
    /// Not populated by current code (stays 0).
    pub affected_active: usize,
    /// Not populated by current code (stays 0).
    pub retained_archives: usize,
    /// Unix time (seconds) when observation started.
    pub observed_unix: u64,
    /// User-facing explanation and caveats of the estimate.
    pub note: String,
    #[serde(default)]
    /// Bytes currently in the local spool.
    pub spool_bytes: u64,
    #[serde(default)]
    /// Local spool limit in bytes.
    pub spool_limit_bytes: u64,
    #[serde(default)]
    /// Number of pending (unpublished) writes.
    pub pending_writes: usize,
    #[serde(skip)]
    /// Ids of pending writes (not serialized).
    pub pending_ids: Vec<String>,
    #[serde(skip)]
    /// Ids of known namespace events (not serialized).
    pub namespace_event_ids: Vec<String>,
    #[serde(default)]
    /// Eligible placement targets with their budgets; reservations reduce their `free`.
    pub targets: Vec<crate::storage::admin::budget::TargetBudget>,
    #[serde(skip)]
    /// Total free placement budget in bytes after reservations (not serialized).
    pub(super) budget: u64,
}

impl CapacityStatus {
    /// Validates the pool and queries every remote's quota through `admin`. Used by
    /// `virtual_drive::capacity`, `mount::upload` and `mount::incremental`.
    pub(crate) fn inspect(admin: &dyn BackendAdmin, policy: &PoolDefinition) -> Result<Self> {
        crate::pool::validate_pool(policy)?;
        let catalog = admin.catalog()?;
        Self::with_catalog(admin, &catalog, policy)
    }

    /// Builds the status from an existing remote catalog and composes the user note.
    fn with_catalog(
        admin: &dyn BackendAdmin,
        catalog: &RemoteCatalog,
        policy: &PoolDefinition,
    ) -> Result<Self> {
        // Timestamp the start of observation, not the end of a slow multi-account query.
        let mut status = Self {
            observed_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            ..Self::default()
        };
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
        status.independent_quota_scenario =
            IndependentQuotaScenario::from_targets(&snapshot.observed_targets, policy)?;
        status.targets = snapshot.targets;
        status.update_account_totals(policy)?;
        status.recalculate(policy)?;
        let feasible = status.additional_estimate > 0;
        status.note = "Planner-verified single-file capacity estimate, not a fixed nominal disk size. Explicitly declared independent account budgets are summed; aliases and unverified accounts share a minimum quota within their group. Small files, metadata/encryption and external changes reduce usable space. OS replica-mode space remains local disk space.".into();
        if status.estimate_limited {
            status.note.push_str(" Simulation limit reached: displayed space is a verified lower bound, not the maximum.");
        }
        if status.targets.iter().any(|t| !t.declared) {
            status.note.insert_str(0, "Capacity groups are missing for some accounts: declare independent accounts below before combining their free space. ");
        }
        if policy.placement == Placement::Resilient
            && status.targets.iter().any(|t| t.failure_domain.is_none())
        {
            status.note.insert_str(0, "Resilient placement requires an outage/failure group for every backing remote; declare those groups before writing. ");
        }
        if let Some(note) = policy.placement.protection_note() {
            status.note.insert_str(0, &format!("{note} "));
        }
        if !feasible {
            if let Err(reason) = status.check_upload(policy, 1) {
                status
                    .note
                    .insert_str(0, &format!("Placement blocker: {reason}. "));
            }
            status.note.insert_str(
                0,
                "Capacity search found no feasible nonempty file within current quotas/outage constraints. This is not proof that every possible file size is infeasible. ",
            );
        }
        Ok(status)
    }

    /// Aggregates targets into per-account totals and the physical/logical sums.
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
        self.nominal_logical_upper = coding_ratio(self.physical_total, policy);
        self.remaining_logical_upper = coding_ratio(self.physical_free, policy);
        Ok(())
    }

    /// Computes the outage-group bound for resilient placement (`resilient_remaining_upper`).
    fn update_outage_bound(&mut self, policy: &PoolDefinition) -> Result<()> {
        self.resilient_remaining_upper = None;
        self.eligible_failure_groups = 0;
        self.required_failure_groups = 0;
        if policy.placement == Placement::Resilient && policy.parity_shards > 0 {
            self.required_failure_groups =
                (policy.data_shards + policy.parity_shards).div_ceil(policy.parity_shards);
            let groups: BTreeSet<_> = self
                .targets
                .iter()
                .filter_map(|target| target.failure_domain.as_deref())
                .collect();
            self.eligible_failure_groups = groups.len();
            if self
                .targets
                .iter()
                .all(|target| target.failure_domain.is_some())
            {
                let mut domain_failures = BTreeMap::<String, BTreeSet<String>>::new();
                for target in &self.targets {
                    domain_failures
                        .entry(target.capacity_domain.clone())
                        .or_default()
                        .insert(target.failure_domain.as_ref().unwrap().clone());
                }
                // A shared quota exposed by targets in several failure groups
                // cannot be charged exclusively to any one of them.
                let mut exclusive_free = BTreeMap::<String, u64>::new();
                for (domain, free) in (crate::storage::admin::budget::BudgetSnapshot {
                    targets: self.targets.clone(),
                    rejected: vec![],
                    observed_targets: vec![],
                })
                .budgets()
                {
                    if let Some(failures) = domain_failures.get(&domain) {
                        if failures.len() == 1 {
                            let failure = failures.iter().next().unwrap().clone();
                            let entry = exclusive_free.entry(failure).or_default();
                            *entry = entry
                                .checked_add(free)
                                .context("outage-group quota overflow")?;
                        }
                    }
                }
                let largest = exclusive_free.values().copied().max().unwrap_or(0);
                // Fewer groups than a full K+M stripe needs can still fit a
                // partial final stripe. Do not report a false zero bound.
                self.resilient_remaining_upper = Some(
                    coding_ratio(self.budget, policy).min(self.budget.saturating_sub(largest)),
                );
            }
        }
        Ok(())
    }

    /// Recomputes `additional_estimate` by binary search over whole stripe groups (capped at
    /// `MAX_SIMULATED_SHARDS`), then over a partial final group, using [`check_upload`](Self::check_upload).
    pub(crate) fn recalculate(&mut self, policy: &PoolDefinition) -> Result<()> {
        self.update_outage_bound(policy)?;
        let shard = policy.shard_bytes()?.get();
        let k = if policy.parity_shards == 0 {
            1
        } else {
            policy.data_shards as u64
        };
        let m = policy.parity_shards as u64;
        let data_group = shard.checked_mul(k).context("data group overflow")?;
        let group = shard.checked_mul(k + m).context("group overflow")?;
        let upper = self.budget / group;
        let cap = MAX_SIMULATED_SHARDS / (k + m);
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
    /// Reserves uploads already in progress: only their planned shards that
    /// are not uploaded yet, each on its planned account. The uploaded ones
    /// are already in the accounts' measured usage; reserving the whole file
    /// as well counted them twice (a 72 GB file half uploaded took 108 GB
    /// off the free space). Unknown accounts (no longer eligible) are
    /// skipped: such a plan is not resumed, its intent is reserved in full.
    pub(crate) fn reserve_remaining(&mut self, remaining: &[(String, u64)]) -> Result<()> {
        for (remote, size) in remaining {
            let Some(domain) = self
                .targets
                .iter()
                .find(|t| &t.remote == remote)
                .map(|t| t.capacity_domain.clone())
            else {
                continue;
            };
            for t in &mut self.targets {
                if t.capacity_domain == domain {
                    t.free = t.free.saturating_sub(*size);
                }
            }
            self.pending_physical_reservation = self
                .pending_physical_reservation
                .checked_add(*size)
                .context("pending reservation overflow")?;
        }
        Ok(())
    }

    /// Reserves full placements for pending writes without a resumable plan, then recomputes the
    /// budget and estimate. Placement failure reports zero additional space instead of an error.
    pub(crate) fn reserve_pending(&mut self, policy: &PoolDefinition, sizes: &[u64]) -> Result<()> {
        if sizes.is_empty() && self.pending_physical_reservation == 0 {
            self.logical_ceiling_estimate =
                self.logical_used.saturating_add(self.additional_estimate);
            return Ok(());
        }
        let shard = policy.shard_bytes()?.get();
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
                observed_targets: vec![],
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
            observed_targets: vec![],
        }
        .total_free()?;
        self.recalculate(policy)
    }

    /// Fails unless a file of `size` logical bytes fits the current budget with full parity and
    /// can be placed by the pool's placement rule. Used before every upload.
    pub(crate) fn check_upload(&self, policy: &PoolDefinition, size: u64) -> Result<()> {
        if self.eligible.is_empty() {
            bail!("No quota-known upload targets; local changes are retained");
        }
        let physical = physical_bytes(policy, size)?;
        if physical > self.budget {
            bail!("Insufficient conservative quota budget including full parity shards; local changes retained");
        }
        let shard = policy.shard_bytes()?.get();
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
                observed_targets: vec![],
            },
            &specs,
            policy.placement,
            coding.as_ref().map(|c| c.parity_shards),
        )?;
        Ok(())
    }
}

/// Physical bytes a file of `size` occupies: data plus full parity shards for every stripe
/// group (partial last shard counted as a full parity shard).
pub(crate) fn physical_bytes(policy: &PoolDefinition, size: u64) -> Result<u64> {
    if size == 0 || policy.parity_shards == 0 {
        return Ok(size);
    }
    let shard = policy.shard_bytes()?.get();
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
            max_object_bytes: None,
            remotes: vec!["x:".into(), "y:".into(), "z:".into(), "unknown:".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
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
            max_object_bytes: None,
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
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
        let scenario = s.independent_quota_scenario.as_ref().unwrap();
        // x: and y: resolve to the same backing; unknown: is only a what-if.
        assert_eq!(scenario.physical_total, 200 * 1048576);
        assert_eq!(scenario.nominal_logical_upper, 160 * 1048576);
    }
    #[test]
    fn mixed_declared_and_unknown_quotas_keep_admission_conservative() {
        let a = admin();
        let mut catalog = a.catalog().unwrap();
        catalog.set_domains(crate::storage::admin::domains::DomainStore {
            version: 1,
            remotes: BTreeMap::from([(
                "a".into(),
                crate::storage::admin::domains::DomainIdentity {
                    capacity: "account-a".into(),
                    failure: String::new(),
                },
            )]),
        });
        let s = CapacityStatus::with_catalog(&a, &catalog, &policy()).unwrap();
        assert_eq!(s.physical_total, 100 * 1048576);
        assert_eq!(s.targets.len(), 2); // x:/y: are one declared account.
        assert!(s.excluded.iter().any(|x| x.remote == "unknown:"));
        let scenario = s.independent_quota_scenario.as_ref().unwrap();
        assert_eq!(scenario.physical_total, 200 * 1048576);
        assert_eq!(scenario.remaining_logical_upper, 128 * 1048576);
        assert_eq!(s.budget, 80 * 1048576);
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
        a.report.total = Some(300000 * 1048576);
        a.report.free = a.report.total;
        a.report.used = Some(0);
        let mut p = policy();
        p.parity_shards = 0;
        let s = CapacityStatus::inspect(&a, &p).unwrap();
        let total = 300000 * 1048576;
        assert_eq!(
            s.nominal_logical_upper,
            total - crate::storage::admin::budget::metadata_reserve(total)
        );
        assert_eq!(s.additional_estimate, MAX_SIMULATED_SHARDS * 1048576);
        assert!(s.estimate_limited);
    }
    #[test]
    fn default_64_mib_shards_report_multi_tib_pools_without_truncation() {
        // 5 TiB free exceeded the old 65,536-shard search (4 TiB at 64 MiB).
        let tib = 1u64 << 40;
        let mut status = CapacityStatus {
            targets: (0..5)
                .map(|i| crate::storage::admin::budget::TargetBudget {
                    remote: format!("p{i}:"),
                    backing: format!("p{i}"),
                    capacity_domain: format!("p{i}"),
                    failure_domain: Some(format!("p{i}")),
                    declared: true,
                    total: 2 * tib,
                    free: tib,
                })
                .collect(),
            ..Default::default()
        };
        let p = PoolDefinition {
            remotes: (0..5).map(|i| format!("p{i}:")).collect(),
            parity_shards: 0,
            placement: Placement::FreeRatio,
            ..Default::default()
        };
        assert_eq!(p.shard_bytes().unwrap().get(), 64 * 1048576);
        status.eligible = status.targets.iter().map(|t| t.remote.clone()).collect();
        status.update_account_totals(&p).unwrap();
        status.budget = status.physical_free;
        status.recalculate(&p).unwrap();
        assert!(!status.estimate_limited);
        assert_eq!(status.additional_estimate, 5 * tib);
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
        assert!(s.note.contains("outage/failure group"));
        assert_eq!(p.placement, Placement::Resilient);
    }

    #[test]
    fn outage_aware_bound_exposes_two_gib_vs_two_tib_bottleneck() {
        let gib = 1u64 << 30;
        let tib = 1u64 << 40;
        let mut status = CapacityStatus {
            targets: [
                ("huge", 2 * tib),
                ("small-a", 2 * gib),
                ("small-b", 2 * gib),
                ("small-c", 2 * gib),
            ]
            .into_iter()
            .map(
                |(name, bytes)| crate::storage::admin::budget::TargetBudget {
                    remote: format!("{name}:"),
                    backing: name.into(),
                    capacity_domain: name.into(),
                    failure_domain: Some(name.into()),
                    declared: true,
                    total: bytes,
                    free: bytes,
                },
            )
            .collect(),
            ..Default::default()
        };
        let p = PoolDefinition {
            max_object_bytes: None,
            data_shards: 3,
            parity_shards: 1,
            shard_size: crate::models::shard_size::ShardSize::from_mib(1024).unwrap(),
            placement: Placement::Resilient,
            ..policy()
        };
        status.eligible = status.targets.iter().map(|t| t.remote.clone()).collect();
        status.update_account_totals(&p).unwrap();
        status.budget = status.physical_free;
        status.update_outage_bound(&p).unwrap();
        assert_eq!(status.physical_total, 2 * tib + 6 * gib);
        assert_eq!(status.required_failure_groups, 4);
        assert_eq!(status.eligible_failure_groups, 4);
        assert_eq!(status.resilient_remaining_upper, Some(6 * gib));
        assert!(status.remaining_logical_upper > 6 * gib);
        assert!(status.check_upload(&p, 9 * gib).is_err());
        let relaxed = PoolDefinition {
            max_object_bytes: None,
            placement: Placement::CapacityFirst,
            ..p.clone()
        };
        assert!(status.check_upload(&relaxed, 9 * gib).is_ok());
        status.update_outage_bound(&relaxed).unwrap();
        assert_eq!(status.resilient_remaining_upper, None);
        assert_eq!(status.required_failure_groups, 0);
        status.recalculate(&relaxed).unwrap();
        assert!(status.additional_estimate > 6 * gib);

        status.targets.truncate(2);
        status.update_account_totals(&p).unwrap();
        status.budget = status.physical_free;
        status.update_outage_bound(&p).unwrap();
        assert_eq!(status.required_failure_groups, 4);
        assert_eq!(status.eligible_failure_groups, 2);
        assert_eq!(status.resilient_remaining_upper, Some(2 * gib));
        status.check_upload(&p, gib).unwrap(); // One data shard + parity fits.
    }
}
