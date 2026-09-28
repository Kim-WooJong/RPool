use crate::models::{Placement, PoolDefinition};
use crate::pool::upsert_pool;
use anyhow::Result;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    rclone: &str,
    name: String,
    remotes: Vec<String>,
    shard_mib: u64,
    workers: usize,
    retries: u32,
    placement: Placement,
    data_shards: usize,
    parity_shards: usize,
) -> Result<()> {
    let pool = PoolDefinition {
        remotes,
        shard_mib,
        workers,
        retries,
        placement,
        data_shards,
        parity_shards,
    };
    let path = upsert_pool(rclone, &name, pool)?;
    println!("pool={name}");
    println!("config={}", path.display());
    Ok(())
}
