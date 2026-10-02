//! `rpool verify`: checks that every physical shard of an archive exists with
//! the right size, or with `--full` streams it and checks BLAKE3.
use crate::manifest::{data_shards, load_manifest_with_storage, validate_manifest};
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use crate::utils::ensure_positive;

/// CLI entry for `rpool verify` through rclone. Called by `application::dispatch`.
pub(crate) fn verify(rclone: &str, manifest_src: &str, full: bool, workers: usize) -> Result<()> {
    verify_with_storage(&StorageReader::rclone(rclone), manifest_src, full, workers)
}

/// Verify with a caller-supplied reader. Used by pool reprocessing and the
/// migration retire step to check archives before relying on them.
pub(crate) fn verify_with_storage(
    reader: &StorageReader,
    manifest_src: &str,
    full: bool,
    workers: usize,
) -> Result<()> {
    verify_shards(reader, manifest_src, full, workers, false)
}

/// Full verification of an archive this process just uploaded or verified:
/// unchanged objects already read back in full are not downloaded again
/// (`StorageReader::verify_unchanged`). Not for the `verify` command.
pub(crate) fn reverify_with_storage(
    reader: &StorageReader,
    manifest_src: &str,
    workers: usize,
) -> Result<()> {
    verify_shards(reader, manifest_src, true, workers, true)
}

/// Verifies all shards in parallel (`workers` threads), logging each failure;
/// `reuse` skips objects this process already read back in full. Fails if any shard is bad.
fn verify_shards(
    reader: &StorageReader,
    manifest_src: &str,
    full: bool,
    workers: usize,
    reuse: bool,
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
                let result = if reuse {
                    reader.verify_unchanged(shard)
                } else {
                    reader.verify(shard, full)
                };
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
