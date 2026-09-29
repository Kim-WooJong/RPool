use crate::pool::load_pool_store;
use anyhow::Result;

pub(crate) fn run(name: &str, json: bool) -> Result<()> {
    let store = load_pool_store()?;
    let pool = store
        .pools
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(pool)?);
        return Ok(());
    }

    println!("name={name}");
    println!("shard_mib={}", pool.shard_size);
    println!("workers={}", pool.workers);
    println!("retries={}", pool.retries);
    println!("placement={}", pool.placement.cli_value());
    println!("data_shards={}", pool.data_shards);
    println!("parity_shards={}", pool.parity_shards);
    for remote in &pool.remotes {
        println!("remote={remote}");
    }
    Ok(())
}
