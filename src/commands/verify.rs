use crate::manifest::{data_shards, load_manifest_with_storage, validate_manifest};
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use crate::utils::ensure_positive;

pub(crate) fn verify(rclone: &str, manifest_src: &str, full: bool, workers: usize) -> Result<()> {
    verify_with_storage(&StorageReader::rclone(rclone), manifest_src, full, workers)
}

pub(crate) fn verify_with_storage(
    reader: &StorageReader,
    manifest_src: &str,
    full: bool,
    workers: usize,
) -> Result<()> {
    ensure_positive(workers, "workers")?;
    let manifest = load_manifest_with_storage(reader, manifest_src)?;
    validate_manifest(&manifest)?;

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;

    let results: Vec<(u32, ShardKind, Result<()>)> = pool.install(|| {
        manifest
            .shards
            .par_iter()
            .map(|shard| {
                let result = reader.verify(shard, full);
                (shard.index, shard.kind, result)
            })
            .collect()
    });

    let mut failures = 0usize;
    for (index, kind, result) in results {
        if let Err(error) = result {
            failures += 1;
            eprintln!("[bad] {:?} shard {index:08}: {error:#}", kind);
        }
    }

    if failures > 0 {
        bail!("verification failed for {failures} physical shard(s)");
    }

    let data_count = data_shards(&manifest).len();
    let parity_count = manifest.shards.len().saturating_sub(data_count);
    println!(
        "ok={} data={} parity={} mode={}",
        manifest.shards.len(),
        data_count,
        parity_count,
        if full { "full-blake3" } else { "quick-size" }
    );
    Ok(())
}
