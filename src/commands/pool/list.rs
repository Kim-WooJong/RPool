use crate::pool::load_pool_store;
use anyhow::Result;

pub(crate) fn run(json: bool) -> Result<()> {
    let store = load_pool_store()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&store)?);
        return Ok(());
    }

    if store.pools.is_empty() {
        println!("no pools configured");
        return Ok(());
    }

    for (name, pool) in store.pools {
        println!(
            "{} remotes={} shard_mib={} workers={} coding={}+{} placement={}",
            name,
            pool.remotes.len(),
            pool.shard_size,
            pool.workers,
            pool.data_shards,
            pool.parity_shards,
            pool.placement.cli_value(),
        );
    }
    Ok(())
}
