use crate::erasure::validate_rs_counts;
use crate::models::PoolDefinition;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

pub(crate) fn validate_pool_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("pool name cannot be empty");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("pool name may contain only ASCII letters, numbers, '.', '-' and '_'");
    }
    Ok(())
}

pub(crate) fn validate_pool(pool: &PoolDefinition) -> Result<()> {
    if pool.remotes.is_empty() {
        bail!("pool requires at least one remote");
    }
    if pool.shard_mib == 0 {
        bail!("pool shard_mib must be greater than zero");
    }
    if pool.workers == 0 {
        bail!("pool workers must be greater than zero");
    }

    let mut unique = BTreeSet::new();
    for remote in &pool.remotes {
        if remote.trim().is_empty() {
            bail!("pool contains an empty remote");
        }
        if !unique.insert(remote) {
            bail!("pool contains duplicate remote: {remote}");
        }
    }

    if pool.parity_shards > 0 {
        validate_rs_counts(pool.data_shards, pool.parity_shards)?;
    }
    Ok(())
}
