use crate::planning::shard_from_plan;
use crate::prelude::*;
use crate::storage::writer::StorageWriter;
use crate::utils::hash_file_range;

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
