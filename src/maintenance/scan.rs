//! Shard integrity scan (scrub) of one archive.

use crate::maintenance::{analyze_groups, group_recoverability};
use crate::prelude::*;
use crate::progress;
use crate::storage::reader::StorageReader;
use crate::utils::ensure_positive;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Probes every shard with `workers` threads (`full` = download and BLAKE3
/// check, else size only), reporting item progress. Returns the counted
/// [`ScrubReport`] with group health and the raw probes for a later repair.
/// Used by `scrub`, `repair` and migrations.
pub(crate) fn scan_manifest_with_storage(
    reader: &StorageReader,
    manifest: &Manifest,
    full: bool,
    workers: usize,
) -> Result<(ScrubReport, Vec<(Shard, Probe)>)> {
    ensure_positive(workers, "workers")?;
    let total = manifest.shards.len();
    progress::items(0, total);
    let completed = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;
    let probes: Vec<(Shard, Probe)> = pool.install(|| {
        manifest
            .shards
            .par_iter()
            .map(|shard| {
                let probe = reader.probe(shard, full);
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                progress::items(done, total);
                (shard.clone(), probe)
            })
            .collect()
    });

    let mut healthy = 0usize;
    let mut missing = 0usize;
    let mut bad_size = 0usize;
    let mut corrupt = 0usize;
    let mut errors = 0usize;
    for (_, probe) in &probes {
        match probe {
            Probe::Ok => healthy += 1,
            Probe::Missing => missing += 1,
            Probe::BadSize { .. } => bad_size += 1,
            Probe::Corrupt { .. } => corrupt += 1,
            Probe::Error(_) => errors += 1,
        }
    }

    let groups = analyze_groups(manifest, &probes);
    let (degraded_groups, unrecoverable_groups) = group_recoverability(manifest, &probes);
    let shards = probes
        .iter()
        .map(|(shard, probe)| ShardHealth::from_probe(shard, probe))
        .collect();

    progress::finish();
    Ok((
        ScrubReport {
            archive_id: manifest.archive_id.clone(),
            mode: if full { "full-blake3" } else { "quick-size" }.to_string(),
            total,
            healthy,
            missing,
            bad_size,
            corrupt,
            errors,
            degraded_groups,
            unrecoverable_groups,
            groups,
            shards,
        },
        probes,
    ))
}
