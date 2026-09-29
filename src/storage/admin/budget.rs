use super::{BackendAdmin, RemoteCatalog};
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TargetBudget {
    pub remote: String,
    pub backing: String,
    pub capacity_domain: String,
    pub failure_domain: Option<String>,
    pub declared: bool,
    pub total: u64,
    pub free: u64,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct BudgetSnapshot {
    pub targets: Vec<TargetBudget>,
    pub rejected: Vec<(String, String, bool)>,
}
impl BudgetSnapshot {
    pub(crate) fn query(
        admin: &dyn BackendAdmin,
        catalog: &RemoteCatalog,
        remotes: &[String],
    ) -> Self {
        let mut result = Self::default();
        let mut reports = BTreeMap::new();
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
                Ok(backing) => result.targets.push(TargetBudget {
                    remote: remote.clone(),
                    backing,
                    capacity_domain: domain.as_str().into(),
                    failure_domain: binding.failure_domain.map(|d| d.as_str().to_owned()),
                    declared: catalog.identity_declared(remote),
                    total,
                    free,
                }),
                Err(e) => {
                    result
                        .rejected
                        .push((remote.clone(), format!("Placement target: {e}"), false))
                }
            }
        }
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
        let unknown = BudgetSnapshot::query(&admin, &catalog, &remotes);
        assert_eq!(unknown.total_free().unwrap(), 2u64 << 30);

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
        assert_eq!(declared.total_free().unwrap(), (2u64 << 40) + (2u64 << 30));
        assert_eq!(declared.budgets().len(), 2); // The crypt alias adds no quota.
    }
}
