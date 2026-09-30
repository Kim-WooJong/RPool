//! Shared size/transfer estimates for copy-only re-encoding (the formula of
//! `pool plan-reprocess`), reused by the migration planner.
use crate::prelude::*;

/// Cloud bytes of a new archive of `size` plaintext bytes under `target`:
/// the data plus one full shard per parity shard of every group.
pub(crate) fn storage_bytes(size: u64, target: &PoolDefinition) -> Result<u64> {
    let shard = target.shard_bytes()?.get();
    if target.parity_shards == 0 {
        return Ok(size);
    }
    if size == 0 {
        bail!("empty files cannot be reprocessed into Reed-Solomon archives");
    }
    let data = size.div_ceil(shard);
    let groups = data.div_ceil(target.data_shards as u64);
    // Every parity shard is a full shard, even for the final partial group.
    groups
        .checked_mul(target.parity_shards as u64)
        .and_then(|v| v.checked_mul(shard))
        .and_then(|v| size.checked_add(v))
        .context("storage estimate overflow")
}

/// (download, upload) of a full restore plus re-`put`: the original is read
/// once, every new shard is read back by the writer and once more by the final
/// full verification.
pub(crate) fn reencode_transfer(input_bytes: u64, new_storage_bytes: u64) -> Result<(u64, u64)> {
    let download = new_storage_bytes
        .checked_mul(2)
        .and_then(|v| input_bytes.checked_add(v))
        .context("transfer estimate overflow")?;
    Ok((download, new_storage_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reprocess_formula() {
        let target = PoolDefinition {
            remotes: vec!["a:".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
            data_shards: 2,
            parity_shards: 2,
            ..Default::default()
        };
        assert_eq!(storage_bytes(1, &target).unwrap(), 1 + 2 * 1048576);
        assert_eq!(
            storage_bytes(3 * 1048576 + 1, &target).unwrap(),
            3 * 1048576 + 1 + 2 * 2 * 1048576
        );
        assert!(storage_bytes(0, &target).is_err());
        let plain = PoolDefinition {
            parity_shards: 0,
            ..target
        };
        assert_eq!(storage_bytes(123, &plain).unwrap(), 123);
        assert_eq!(reencode_transfer(10, 20).unwrap(), (50, 20));
    }
}
