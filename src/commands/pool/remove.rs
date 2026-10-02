//! `rpool pool remove`: deletes a pool definition (stored shards are not touched).
use crate::pool::remove_pool;
use anyhow::Result;

/// Removes pool `name` from the pool store and prints the config file path.
pub(crate) fn run(name: &str) -> Result<()> {
    let path = remove_pool(name)?;
    println!("removed={name}");
    println!("config={}", path.display());
    Ok(())
}
