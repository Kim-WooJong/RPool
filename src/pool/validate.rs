//! Validation of pool names and pool definitions before they are saved or used.
use crate::erasure::validate_rs_counts;
use crate::models::PoolDefinition;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

/// Accepts non-empty names made of ASCII letters, digits, `.`, `-` and `_`, so
/// the name is safe in file paths and remote paths. Used by pool management,
/// migration journals, mounts and config import.
pub(crate) fn validate_pool_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("pool name cannot be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("pool name may contain only ASCII letters, numbers, '.', '-' and '_'");
    }
    Ok(())
}

/// Checks a pool definition for consistency: at least one unique non-empty
/// remote, a valid shard size whose encrypted object fits `max_object_bytes`,
/// non-zero workers, parity for resilient placement and valid RS counts.
/// Called before a pool is used by uploads, mounts, migrations and doctor.
pub(crate) fn validate_pool(pool: &PoolDefinition) -> Result<()> {
    if pool.remotes.is_empty() {
        bail!("pool requires at least one remote");
    }
    let shard = pool
        .shard_bytes()
        .map_err(|e| anyhow::anyhow!("invalid pool shard_mib: {e}"))?;
    crate::models::shard_size::check_object_limit(shard.get(), pool.max_object_bytes)
        .map_err(|e| anyhow::anyhow!("invalid pool shard size: {e}"))?;
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

    if pool.placement == crate::models::Placement::Resilient && pool.parity_shards == 0 {
        bail!("resilient placement requires parity shards");
    }
    if pool.parity_shards > 0 {
        validate_rs_counts(pool.data_shards, pool.parity_shards)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::shard_size::{crypt_size, ShardSize, MIB};

    fn pool(mib: u64, limit: Option<u64>) -> PoolDefinition {
        PoolDefinition {
            remotes: vec!["a-crypt:".into(), "b-crypt:".into()],
            shard_size: ShardSize::from_mib(mib).unwrap(),
            data_shards: 2,
            parity_shards: 1,
            max_object_bytes: limit,
            ..PoolDefinition::default()
        }
    }

    #[test]
    fn pool_object_limit_uses_encrypted_shard_size() {
        validate_pool(&pool(64, None)).unwrap();
        validate_pool(&pool(64, Some(250_000_000))).unwrap();
        validate_pool(&pool(220, Some(250_000_000))).unwrap();
        validate_pool(&pool(64, Some(crypt_size(64 * MIB)))).unwrap();
        // Plaintext fits but the crypt object does not: must be rejected.
        let error = validate_pool(&pool(64, Some(64 * MIB)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("max_object_bytes"), "{error}");
        assert!(validate_pool(&pool(239, Some(250_000_000))).is_err());
        assert!(validate_pool(&pool(64, Some(0))).is_err());
    }
}
