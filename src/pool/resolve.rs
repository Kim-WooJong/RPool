use crate::config::constants::*;
use crate::models::{Placement, ResolvedPutOptions};
use crate::pool::{load_pool_store, validate_pool};
use crate::remote_root::apply_remote_roots;
use anyhow::{bail, Result};

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_put_options(
    pool_name: Option<&str>,
    explicit_remotes: Vec<String>,
    shard_mib: Option<u64>,
    workers: Option<usize>,
    placement: Option<Placement>,
    retries: Option<u32>,
    data_shards: Option<usize>,
    parity_shards: Option<usize>,
) -> Result<ResolvedPutOptions> {
    if pool_name.is_some() && !explicit_remotes.is_empty() {
        bail!("--pool and --remote cannot be used together");
    }

    let pool = if let Some(name) = pool_name {
        let store = load_pool_store()?;
        let pool = store
            .pools
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))?;
        validate_pool(&pool)?;
        Some(pool)
    } else {
        None
    };

    let remotes = if let Some(pool) = &pool {
        apply_remote_roots(pool.remotes.clone())?
    } else {
        apply_remote_roots(explicit_remotes)?
    };
    if remotes.is_empty() {
        bail!("provide at least one --remote or select --pool");
    }

    Ok(ResolvedPutOptions {
        remotes,
        shard_mib: shard_mib.unwrap_or_else(|| {
            pool.as_ref()
                .map(|p| p.shard_mib)
                .unwrap_or(DEFAULT_SHARD_MIB)
        }),
        workers: workers
            .unwrap_or_else(|| pool.as_ref().map(|p| p.workers).unwrap_or(DEFAULT_WORKERS)),
        retries: retries
            .unwrap_or_else(|| pool.as_ref().map(|p| p.retries).unwrap_or(DEFAULT_RETRIES)),
        placement: placement.unwrap_or_else(|| {
            pool.as_ref()
                .map(|p| p.placement)
                .unwrap_or(Placement::RoundRobin)
        }),
        data_shards: data_shards.unwrap_or_else(|| {
            pool.as_ref()
                .map(|p| p.data_shards)
                .unwrap_or(DEFAULT_DATA_SHARDS)
        }),
        parity_shards: parity_shards.unwrap_or_else(|| {
            pool.as_ref()
                .map(|p| p.parity_shards)
                .unwrap_or(DEFAULT_PARITY_SHARDS)
        }),
        pool_name: pool_name.map(ToOwned::to_owned),
    })
}
