//! `rpool pool set`: creates or replaces a pool definition.
use crate::models::{Placement, PoolDefinition};
use crate::pool::upsert_pool;
use anyhow::Result;

#[allow(clippy::too_many_arguments)]
/// Saves pool `name` with the given options via `pool::upsert_pool` and hints at
/// `pool migrate plan` when the change affects already stored archives.
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
    max_object_bytes: Option<u64>,
    native_crypt: bool,
    small_file_packing: bool,
) -> Result<()> {
    let pool = PoolDefinition {
        remotes,
        shard_size: crate::models::shard_size::ShardSize::from_mib(shard_mib)?,
        workers,
        retries,
        placement,
        data_shards,
        parity_shards,
        max_object_bytes,
        native_crypt,
        small_file_packing,
    };
    let previous = crate::pool::load_pool_store()
        .ok()
        .and_then(|store| store.pools.get(&name).cloned());
    let affects = previous
        .as_ref()
        .is_some_and(|old| affects_stored_data(old, &pool));
    let path = upsert_pool(rclone, &name, pool)?;
    println!("pool={name}");
    println!("config={}", path.display());
    if affects {
        println!("Stored archives are affected: run `rpool pool migrate plan {name}`");
    }
    Ok(())
}

/// Whether archives written under `old` no longer match `new`: a remote was
/// removed, or K, M, shard size or native crypt changed. Adding remotes or
/// changing workers/retries/placement alone moves nothing.
pub(crate) fn affects_stored_data(old: &PoolDefinition, new: &PoolDefinition) -> bool {
    old.remotes.iter().any(|r| !new.remotes.contains(r))
        || old.data_shards != new.data_shards
        || old.parity_shards != new.parity_shards
        || old.shard_bytes().ok() != new.shard_bytes().ok()
        || old.native_crypt != new.native_crypt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_data_affecting_changes_hint_migration() {
        let old = PoolDefinition {
            remotes: vec!["a:".into(), "b:".into()],
            ..Default::default()
        };
        let mut new = old.clone();
        new.remotes.push("c:".into());
        new.workers += 3;
        new.retries += 1;
        assert!(!affects_stored_data(&old, &new));
        let mut removed = old.clone();
        removed.remotes.pop();
        assert!(affects_stored_data(&old, &removed));
        let mut k = old.clone();
        k.data_shards += 1;
        assert!(affects_stored_data(&old, &k));
        let mut m = old.clone();
        m.parity_shards += 1;
        assert!(affects_stored_data(&old, &m));
        let mut crypt = old.clone();
        crypt.native_crypt = !crypt.native_crypt;
        assert!(affects_stored_data(&old, &crypt));
        let mut shard = old.clone();
        shard.shard_size =
            crate::models::shard_size::ShardSize::from_mib(old.shard_mib().unwrap() * 2).unwrap();
        assert!(affects_stored_data(&old, &shard));
    }
}
