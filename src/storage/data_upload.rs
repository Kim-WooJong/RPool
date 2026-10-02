//! Upload of a single planned data shard.
//!
//! Hashes the source byte range for the shard and writes it through
//! `StorageWriter` (which verifies the upload). Re-exported as
//! `storage::upload_one_data_shard`; exercised by `storage::writer_tests`.

use crate::planning::shard_from_plan;
use crate::prelude::*;
use crate::storage::writer::StorageWriter;
use crate::utils::hash_file_range;

/// Hashes `source[p.offset..p.offset+p.size]`, uploads it as data shard `p`
/// with up to `retries` retries, and returns the resulting `Shard` record.
/// Errors if `p` is a parity shard or the hash/write fails.
pub(crate) fn upload_one_data_shard(
    storage: &StorageWriter,
    source: &Path,
    p: &PlanShard,
    retries: u32,
) -> Result<Shard> {
    if p.kind != ShardKind::Data {
        bail!("internal error: upload_one_data_shard received a parity shard");
    }

    let shard = shard_from_plan(p, hash_file_range(source, p.offset, p.size)?);
    storage.write_file(source, p.offset, &shard, retries)?;
    eprintln!("[verified] data {:08} {}", p.index, p.remote);
    Ok(shard)
}
