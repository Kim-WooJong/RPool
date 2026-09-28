//! Read-only pool capacity: no workspace, mount, inventory scan or config mutation.
use crate::mount::capacity::CapacityStatus;
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PoolCapacity {
    pub policy: PoolDefinition,
    pub namespace_used: Option<u64>,
    pub capacity: CapacityStatus,
}
pub(crate) fn inspect(admin: &dyn BackendAdmin, policy: &PoolDefinition) -> Result<PoolCapacity> {
    let mut capacity = CapacityStatus::inspect(admin, policy)?;
    capacity.usage_scope = "not-queried".into();
    Ok(PoolCapacity {
        policy: policy.clone(),
        namespace_used: None,
        capacity,
    })
}
pub(crate) fn query(rclone: &str, policy: &PoolDefinition) -> Result<PoolCapacity> {
    inspect(&RcloneAdmin::inherited(rclone), policy)
}
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
        policy.shard_mib = n;
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
        println!("Coding-only nominal logical upper bound={} bytes; current remaining upper bound={} bytes", c.nominal_logical_upper, c.remaining_logical_upper);
        println!("Planner-verified additional file estimate={} bytes; simulation_limited={}; namespace_usage=not_queried", c.additional_estimate, c.estimate_limited);
        println!("{}", c.note);
        for excluded in &c.excluded {
            println!("Excluded {}: {}", excluded.remote, excluded.reason);
        }
    }
    Ok(())
}
