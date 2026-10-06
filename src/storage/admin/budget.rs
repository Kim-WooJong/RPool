//! Free-space budget of a set of pool remotes: queries each backing
//! account's quota once, rejects remotes whose quota or identity cannot be
//! trusted, and sums free space per capacity domain without double counting.
//! Entry point `BudgetSnapshot::query`, used by `mount::capacity`,
//! `mount::upload` and `placement`.
use super::{BackendAdmin, RemoteCatalog};
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Quota of one accepted pool remote.
pub(crate) struct TargetBudget {
    /// Pool remote address as given by the caller.
    pub remote: String,
    /// Configured backing section the remote resolves to (see `RemoteCatalog::placement_target`).
    pub backing: String,
    /// Capacity domain; remotes in the same domain share one quota.
    pub capacity_domain: String,
    /// Declared outage domain, if any.
    pub failure_domain: Option<String>,
    /// The backing remote has a user-declared identity in `provider_domains.json`.
    pub declared: bool,
    /// Total quota in bytes reported by the provider, less the
    /// [`metadata_reserve`] (space shards never use).
    pub total: u64,
    /// Free bytes reported by the provider, less the [`metadata_reserve`]:
    /// what shards may still fill.
    pub free: u64,
    /// This PC's lower limit of shards per coding group on this account
    /// (`AccountLimits::max_group_shards`); `None` = the placement's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_cap: Option<usize>,
}

/// This PC's `max_group_shards` of the account behind placement target
/// `backing` (`name:` or `name:path`).
fn group_shard_limit(backing: &str) -> Option<usize> {
    let account = backing.split_once(':').map_or(backing, |(name, _)| name);
    crate::storage::account::runtime::settings()
        .store
        .account(account)
        .and_then(|limits| limits.max_group_shards)
        .map(|n| n as usize)
}

/// Space kept free on every account for the drive's metadata, which goes to
/// every pool account (change records, checkpoints) and must be written to
/// all of them before other PCs see a change: 1/64 of the account, at most
/// 1 GiB (a change record is a few KiB, a checkpoint chunk at most 8 MiB).
/// Accounts below 1 GiB keep nothing. Shard placement and the capacity
/// estimates see the account without it, so shards never fill an account
/// completely and metadata writes keep working when the pool is full.
pub(crate) fn metadata_reserve(total: u64) -> u64 {
    const GIB: u64 = 1 << 30;
    if total < GIB {
        0
    } else {
        (total / 64).min(GIB)
    }
}
#[derive(Debug, Clone, Default)]
/// Result of one quota round over a set of pool remotes.
pub(crate) struct BudgetSnapshot {
    /// Remotes accepted for capacity admission.
    pub targets: Vec<TargetBudget>,
    /// Rejected remotes: (remote, reason, whether the quota query itself failed).
    pub rejected: Vec<(String, String, bool)>,
    /// Valid quota responses before unresolved identities are excluded from admission.
    pub observed_targets: Vec<TargetBudget>,
}
impl BudgetSnapshot {
    /// Quota reports for each distinct backing target, queried concurrently so a
    /// refresh takes about as long as the slowest account, not the sum of all.
    fn quotas(
        admin: &dyn BackendAdmin,
        catalog: &RemoteCatalog,
        remotes: &[String],
    ) -> BTreeMap<String, QuotaReport> {
        const CONCURRENT: usize = 8;
        let targets: BTreeSet<String> = remotes
            .iter()
            .filter_map(|remote| catalog.capacity(remote).ok())
            .map(|binding| binding.target)
            .collect();
        let targets: Vec<String> = targets.into_iter().collect();
        let mut reports = BTreeMap::new();
        for batch in targets.chunks(CONCURRENT) {
            let answers: Vec<QuotaReport> = std::thread::scope(|scope| {
                let workers: Vec<_> = batch
                    .iter()
                    .map(|target| scope.spawn(move || admin.quota(target)))
                    .collect();
                workers
                    .into_iter()
                    .zip(batch)
                    .map(|(worker, target)| {
                        worker.join().unwrap_or_else(|_| {
                            super::unavailable_quota(target, "quota query panicked".into())
                        })
                    })
                    .collect()
            });
            reports.extend(batch.iter().cloned().zip(answers));
        }
        reports
    }

    /// Resolves and quota-checks every remote in `remotes`. Remotes with
    /// unresolved targets, failed or incomplete/inconsistent quotas, or (when
    /// any identity is declared) undeclared identities are moved to `rejected`.
    pub(crate) fn query(
        admin: &dyn BackendAdmin,
        catalog: &RemoteCatalog,
        remotes: &[String],
    ) -> Self {
        let mut result = Self::default();
        let mut reports = Self::quotas(admin, catalog, remotes);
        for remote in remotes {
            let binding = match catalog.capacity(remote) {
                Ok(b) => b,
                Err(e) => {
                    result.rejected.push((
                        remote.clone(),
                        format!("Target resolution: {e}"),
                        false,
                    ));
                    continue;
                }
            };
            let report = reports
                .entry(binding.target.clone())
                .or_insert_with(|| admin.quota(&binding.target));
            if let Some(error) = &report.error {
                result
                    .rejected
                    .push((remote.clone(), format!("Quota query: {error}"), true));
                continue;
            }
            let (Some(total), Some(free)) = (report.total, report.free) else {
                result.rejected.push((
                    remote.clone(),
                    "Backend did not report both total and remaining quota".into(),
                    false,
                ));
                continue;
            };
            if free > total
                || report
                    .used
                    .is_some_and(|u| u.checked_add(free).is_none_or(|sum| sum > total))
            {
                result
                    .rejected
                    .push((remote.clone(), "Inconsistent quota response".into(), false));
                continue;
            }
            let Some(domain) = binding.domain else {
                result
                    .rejected
                    .push((remote.clone(), "Unresolved quota scope".into(), false));
                continue;
            };
            match catalog.placement_target(remote) {
                Ok(backing) => {
                    let reserve = metadata_reserve(total);
                    let shard_cap = group_shard_limit(&backing);
                    result.targets.push(TargetBudget {
                        remote: remote.clone(),
                        backing,
                        capacity_domain: domain.as_str().into(),
                        failure_domain: binding.failure_domain.map(|d| d.as_str().to_owned()),
                        declared: catalog.identity_declared(remote),
                        total: total.saturating_sub(reserve),
                        free: free.saturating_sub(reserve),
                        shard_cap,
                    })
                }
                Err(e) => {
                    result
                        .rejected
                        .push((remote.clone(), format!("Placement target: {e}"), false))
                }
            }
        }
        result.observed_targets = result.targets.clone();
        // Unknown configuration sections might alias any declared account.
        // Never add their budget as though it were independent.
        if result.targets.iter().any(|t| t.declared) {
            let unknown: Vec<_> = result
                .targets
                .iter()
                .filter(|t| !t.declared)
                .map(|t| t.remote.clone())
                .collect();
            result.targets.retain(|t| t.declared);
            result.rejected.extend(unknown.into_iter().map(|remote| (remote, "Account identity unresolved alongside declared accounts; map its quota group before pooling".into(), false)));
        }
        result
    }
    /// Free bytes per capacity domain; remotes sharing a domain count the
    /// smallest free value once.
    pub(crate) fn budgets(&self) -> BTreeMap<String, u64> {
        let mut budgets = BTreeMap::new();
        for target in &self.targets {
            budgets
                .entry(target.capacity_domain.clone())
                .and_modify(|f: &mut u64| *f = (*f).min(target.free))
                .or_insert(target.free);
        }
        budgets
    }
    /// Sum of [`Self::budgets`]; errors on overflow.
    pub(crate) fn total_free(&self) -> Result<u64> {
        self.budgets().values().try_fold(0u64, |sum, n| {
            sum.checked_add(*n).context("quota total overflow")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::admin::domains::{DomainIdentity, DomainStore};
    struct Admin;
    impl BackendAdmin for Admin {
        fn catalog(&self) -> Result<RemoteCatalog> {
            RemoteCatalog::parse(&serde_json::json!({
                "a":{"type":"webdav"},"b":{"type":"koofr"},"alias":{"type":"crypt","remote":"a:"}
            }))
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            QuotaReport {
                remote: remote.into(),
                total: Some(100),
                used: Some(0),
                free: Some(100),
                trashed: None,
                other: None,
                used_percent: None,
                error: None,
            }
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
    /// Seven accounts whose quota query takes 300 ms each.
    struct SlowAdmin(std::sync::atomic::AtomicUsize);
    impl BackendAdmin for SlowAdmin {
        fn catalog(&self) -> Result<RemoteCatalog> {
            let sections: serde_json::Map<String, Value> = (1..=7)
                .map(|i| (format!("p{i}"), serde_json::json!({"type": "webdav"})))
                .collect();
            RemoteCatalog::parse(&Value::Object(sections))
        }
        fn quota(&self, remote: &str) -> QuotaReport {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(300));
            Admin.quota(remote)
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
    #[test]
    fn account_quotas_are_queried_concurrently_once_each() {
        let admin = SlowAdmin(Default::default());
        let catalog = admin.catalog().unwrap();
        let mut remotes: Vec<String> = (1..=7).map(|i| format!("p{i}:")).collect();
        remotes.push("p1:".into());
        let started = std::time::Instant::now();
        let snapshot = BudgetSnapshot::query(&admin, &catalog, &remotes);
        let elapsed = started.elapsed();
        assert_eq!(snapshot.targets.len(), 8);
        assert_eq!(
            admin.0.load(std::sync::atomic::Ordering::SeqCst),
            7,
            "one query per account"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "took {elapsed:?}"
        );
    }
    #[test]
    fn unresolved_account_is_not_additive_with_declared_account() {
        let mut catalog = Admin.catalog().unwrap();
        catalog.set_domains(DomainStore {
            version: 1,
            remotes: BTreeMap::from([(
                "a".into(),
                DomainIdentity {
                    capacity: "account".into(),
                    failure: String::new(),
                },
            )]),
        });
        let snapshot = BudgetSnapshot::query(
            &Admin,
            &catalog,
            &["a:".into(), "b:".into(), "alias:".into()],
        );
        assert_eq!(snapshot.total_free().unwrap(), 100);
        assert_eq!(snapshot.targets.len(), 2);
        assert_eq!(snapshot.rejected.len(), 1);
        assert_eq!(snapshot.rejected[0].0, "b:");
    }
    #[test]
    fn explicit_independent_budgets_combine_but_aliases_do_not() {
        let mut c = Admin.catalog().unwrap();
        let remotes = vec!["a:".into(), "b:".into(), "alias:".into()];
        assert_eq!(
            BudgetSnapshot::query(&Admin, &c, &remotes)
                .total_free()
                .unwrap(),
            100
        );
        c.set_domains(DomainStore {
            version: 1,
            remotes: BTreeMap::from([
                (
                    "a".into(),
                    DomainIdentity {
                        capacity: "account-a".into(),
                        failure: "provider".into(),
                    },
                ),
                (
                    "b".into(),
                    DomainIdentity {
                        capacity: "account-b".into(),
                        failure: "provider".into(),
                    },
                ),
            ]),
        });
        let s = BudgetSnapshot::query(&Admin, &c, &remotes);
        assert!(s.rejected.is_empty());
        assert_eq!(s.total_free().unwrap(), 200);
        assert_eq!(s.targets[0].capacity_domain, s.targets[2].capacity_domain);
        assert_eq!(s.targets[0].failure_domain, s.targets[1].failure_domain);
    }

    #[test]
    fn two_gib_and_two_tib_are_summed_only_when_independence_is_declared() {
        struct SkewAdmin;
        impl BackendAdmin for SkewAdmin {
            fn catalog(&self) -> Result<RemoteCatalog> {
                Admin.catalog()
            }
            fn quota(&self, remote: &str) -> QuotaReport {
                let bytes = if remote == "b:" {
                    2u64 << 40
                } else {
                    2u64 << 30
                };
                QuotaReport {
                    remote: remote.into(),
                    total: Some(bytes),
                    used: Some(0),
                    free: Some(bytes),
                    trashed: None,
                    other: None,
                    used_percent: None,
                    error: None,
                }
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
        let admin = SkewAdmin;
        let mut catalog = admin.catalog().unwrap();
        let remotes = ["a:".into(), "alias:".into(), "b:".into()];
        // Each account keeps its metadata reserve out of the budget.
        let net = |bytes: u64| bytes - metadata_reserve(bytes);
        let unknown = BudgetSnapshot::query(&admin, &catalog, &remotes);
        assert_eq!(unknown.total_free().unwrap(), net(2u64 << 30));
        assert_eq!(unknown.observed_targets.len(), 3);

        catalog.set_domains(DomainStore {
            version: 1,
            remotes: BTreeMap::from([
                (
                    "a".into(),
                    DomainIdentity {
                        capacity: "small".into(),
                        failure: "small-provider".into(),
                    },
                ),
                (
                    "b".into(),
                    DomainIdentity {
                        capacity: "large".into(),
                        failure: "large-provider".into(),
                    },
                ),
            ]),
        });
        let declared = BudgetSnapshot::query(&admin, &catalog, &remotes);
        assert!(declared.rejected.is_empty());
        assert_eq!(
            declared.total_free().unwrap(),
            net(2u64 << 40) + net(2u64 << 30)
        );
        assert_eq!(declared.budgets().len(), 2); // The crypt alias adds no quota.
    }

    #[test]
    fn metadata_reserve_is_a_64th_of_the_account_up_to_one_gib() {
        const GIB: u64 = 1 << 30;
        assert_eq!(metadata_reserve(0), 0);
        assert_eq!(metadata_reserve(GIB - 1), 0);
        assert_eq!(metadata_reserve(GIB), GIB / 64);
        assert_eq!(metadata_reserve(10 * GIB), 10 * GIB / 64);
        assert_eq!(metadata_reserve(150 * GIB), GIB);
        assert_eq!(metadata_reserve(8 << 40), GIB);
    }

    /// An account with less free space than its reserve takes no shards, but
    /// the pool still lists it (metadata keeps going there).
    #[test]
    fn nearly_full_account_has_no_shard_budget() {
        const GIB: u64 = 1 << 30;
        struct Full;
        impl BackendAdmin for Full {
            fn catalog(&self) -> Result<RemoteCatalog> {
                Admin.catalog()
            }
            fn quota(&self, remote: &str) -> QuotaReport {
                QuotaReport {
                    remote: remote.into(),
                    total: Some(150 * GIB),
                    used: Some(150 * GIB - GIB / 2),
                    free: Some(GIB / 2),
                    trashed: None,
                    other: None,
                    used_percent: None,
                    error: None,
                }
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
        let catalog = Full.catalog().unwrap();
        let s = BudgetSnapshot::query(&Full, &catalog, &["a:".into()]);
        assert!(s.rejected.is_empty());
        assert_eq!(s.targets[0].free, 0);
        assert_eq!(s.targets[0].total, 149 * GIB);
        assert_eq!(s.total_free().unwrap(), 0);
    }
}
