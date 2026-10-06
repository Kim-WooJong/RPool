//! Read-only pool capacity: no workspace, mount, inventory scan or config mutation.
use crate::mount::capacity::CapacityStatus;
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

/// A pool destination and the configured backing section it resolves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackingRemote {
    /// Pool destination (`remote:path`, after remote roots).
    pub remote: String,
    /// rclone section that holds the data for that destination.
    pub backing: String,
    /// rclone backend type of the backing section.
    pub kind: String,
}

/// `rpool pool capacity` report (also shown by the GUI pool capacity widget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PoolCapacity {
    /// Pool definition the report was computed for.
    pub policy: PoolDefinition,
    /// Drive namespace usage; always `None` here (not queried).
    pub namespace_used: Option<u64>,
    /// Account quotas and pool capacity estimates.
    pub capacity: CapacityStatus,
    /// Every resolvable destination, including ones whose quota query failed.
    #[serde(default)]
    pub backings: Vec<BackingRemote>,
}
/// Builds the report for `policy` with `admin` (quota queries only).
pub(crate) fn inspect(admin: &dyn BackendAdmin, policy: &PoolDefinition) -> Result<PoolCapacity> {
    let mut capacity = CapacityStatus::inspect(admin, policy)?;
    capacity.usage_scope = "not-queried".into();
    Ok(PoolCapacity {
        policy: policy.clone(),
        namespace_used: None,
        capacity,
        backings: backings(admin, policy)?,
    })
}
/// Backing sections of the pool's destinations; unresolvable ones are skipped
/// (the capacity status already reports them).
pub(crate) fn backings(
    admin: &dyn BackendAdmin,
    policy: &PoolDefinition,
) -> Result<Vec<BackingRemote>> {
    let catalog = admin.catalog()?;
    let remotes = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
    Ok(remotes
        .into_iter()
        .filter_map(|remote| {
            let backing = catalog.placement_target(&remote).ok()?;
            let kind = catalog
                .backend_kind(&backing)
                .unwrap_or("unknown")
                .to_owned();
            Some(BackingRemote {
                remote,
                backing,
                kind,
            })
        })
        .collect())
}
/// [`inspect`] with the real rclone admin. Used by `run` and the GUI.
pub(crate) fn query(rclone: &str, policy: &PoolDefinition) -> Result<PoolCapacity> {
    inspect(&RcloneAdmin::inherited(rclone), policy)
}
/// `rpool pool capacity`: a saved pool or an ad-hoc definition from the
/// CLI options, printed as text or JSON.
pub(crate) fn run(rclone: &str, args: crate::cli::pool::PoolCapacityArgs) -> Result<()> {
    let mut policy = match args.name {
        Some(name) => super::load_pool_store()?
            .pools
            .get(&name)
            .cloned()
            .context("pool not found")?,
        None => PoolDefinition::default(),
    };
    if !args.remotes.is_empty() {
        policy.remotes = args.remotes;
    }
    if let Some(n) = args.shard_mib {
        policy.shard_size = crate::models::shard_size::ShardSize::from_mib(n)?;
    }
    if let Some(n) = args.data_shards {
        policy.data_shards = n;
    }
    if let Some(n) = args.parity_shards {
        policy.parity_shards = n;
    }
    if let Some(n) = args.placement {
        policy.placement = n;
    }
    let report = query(rclone, &policy)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let c = &report.capacity;
        println!(
            "Account quota (known, deduplicated): total={} occupied={} free={} bytes; complete={}",
            c.physical_total, c.physical_occupied, c.physical_free, c.quota_complete
        );
        println!(
            "Pool data after parity (coding-only upper bound): total={} remaining={} bytes",
            c.nominal_logical_upper, c.remaining_logical_upper
        );
        if let Some(scenario) = &c.independent_quota_scenario {
            println!("If unverified backing accounts are independent (NOT verified/admissible): total={} remaining={} logical bytes", scenario.nominal_logical_upper, scenario.remaining_logical_upper);
        }
        if let Some(b) = c
            .balance
            .as_ref()
            .filter(|b| b.groups.iter().any(|g| g.unusable > 0))
        {
            println!(
                "Uneven groups: at most {} shards of each coding group per group, so data after parity fits up to {} bytes",
                b.per_group_cap, b.usable_logical
            );
            for g in b.groups.iter().filter(|g| g.unusable > 0) {
                println!(
                    "  {}: {} of {} free bytes cannot be filled",
                    g.group, g.unusable, g.free
                );
            }
            if b.add_to_use_all > 0 {
                println!(
                    "  To fill every group, add at least {} bytes in {} or more new group(s), each no larger than the largest",
                    b.add_to_use_all, b.add_groups_min
                );
            }
        }
        println!("Placement-checked next-file estimate={} bytes; simulation_limited={}; namespace_usage=not_queried", c.additional_estimate, c.estimate_limited);
        println!("{}", c.note);
        for excluded in &c.excluded {
            println!("Excluded {}: {}", excluded.remote, excluded.reason);
        }
    }
    Ok(())
}

#[cfg(test)]
mod backing_tests {
    use super::*;
    use crate::storage::admin::RemoteCatalog;

    struct Catalog;
    impl BackendAdmin for Catalog {
        fn catalog(&self) -> Result<RemoteCatalog> {
            RemoteCatalog::parse(&serde_json::json!({
                "dropbox": {"type": "dropbox"},
                "dropbox_crypt": {"type": "crypt", "remote": "dropbox:rpool"},
                "mt": {"type": "s3"},
                "mt_crypt": {"type": "crypt", "remote": "mt:bucket"},
                "both": {"type": "union"}
            }))
        }
        fn quota(&self, _: &str) -> QuotaReport {
            unreachable!("backings never query quotas")
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
    fn destinations_resolve_to_backing_account_and_type_without_quota() {
        let policy = PoolDefinition {
            remotes: vec!["dropbox_crypt:".into(), "mt_crypt:".into(), "both:".into()],
            ..Default::default()
        };
        let found = backings(&Catalog, &policy).unwrap();
        let summary: Vec<(&str, &str)> = found
            .iter()
            .map(|b| (b.backing.as_str(), b.kind.as_str()))
            .collect();
        assert_eq!(
            summary,
            [("dropbox", "dropbox"), ("mt", "s3")],
            "unions are skipped"
        );
    }
}
