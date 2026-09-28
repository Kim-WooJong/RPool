use crate::maintenance::{
    repair_manifest_with_storage, save_integrity_snapshot, scan_manifest_with_storage,
};
use crate::manifest::{load_manifest_with_storage, validate_manifest};
use crate::prelude::*;
use crate::storage::writer::StorageWriter;

#[allow(clippy::too_many_arguments)]
pub(crate) fn scrub(
    rclone: &str,
    manifest_src: &str,
    quick: bool,
    repair: bool,
    dry_run: bool,
    workers: usize,
    retries: u32,
    json: bool,
) -> Result<()> {
    scrub_with_storage(
        &StorageWriter::rclone(rclone),
        manifest_src,
        quick,
        repair,
        dry_run,
        workers,
        retries,
        json,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scrub_with_storage(
    storage: &StorageWriter,
    manifest_src: &str,
    quick: bool,
    repair: bool,
    dry_run: bool,
    workers: usize,
    retries: u32,
    json: bool,
) -> Result<()> {
    if dry_run && !repair {
        bail!("--dry-run is only meaningful together with --repair");
    }
    if json && repair {
        bail!("--json cannot be combined with --repair; run the scrub report and repair as separate operations");
    }
    let reader = storage.reader();
    let manifest = load_manifest_with_storage(&reader, manifest_src)?;
    validate_manifest(&manifest)?;
    let full = !quick;
    let (report, probes) = scan_manifest_with_storage(&reader, &manifest, full, workers)?;
    if !dry_run {
        if let Err(error) = save_integrity_snapshot(manifest_src, &report) {
            eprintln!("[integrity] failed to save local scrub snapshot: {error:#}");
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    let bad = report.total.saturating_sub(report.healthy);
    if repair && bad > 0 {
        if report.errors > 0 {
            bail!("provider errors remain; refusing automatic repair until reads can be verified");
        }
        if report.unrecoverable_groups > 0 {
            bail!(
                "scrub found {} unrecoverable group(s); refusing automatic repair",
                report.unrecoverable_groups
            );
        }
        let repaired =
            repair_manifest_with_storage(&storage, &manifest, &probes, retries, dry_run, None)?;
        println!("repair_candidates={repaired}");
        if !dry_run {
            let (after, _) = scan_manifest_with_storage(&reader, &manifest, true, workers)?;
            if let Err(error) = save_integrity_snapshot(manifest_src, &after) {
                eprintln!("[integrity] failed to save post-repair snapshot: {error:#}");
            }
            if after.healthy != after.total {
                bail!(
                    "post-repair scrub still reports {} bad shard(s)",
                    after.total - after.healthy
                );
            }
            println!("post_repair=healthy");
        }
    }

    if bad > 0 && !repair {
        bail!("scrub found {bad} bad physical shard(s)");
    }
    Ok(())
}

fn print_report(report: &ScrubReport) {
    println!("archive_id={}", report.archive_id);
    println!("mode={}", report.mode);
    println!(
        "total={} healthy={} missing={} bad_size={} corrupt={} errors={}",
        report.total,
        report.healthy,
        report.missing,
        report.bad_size,
        report.corrupt,
        report.errors
    );
    println!(
        "degraded_groups={} unrecoverable_groups={}",
        report.degraded_groups, report.unrecoverable_groups
    );
    for group in &report.groups {
        if group.status != "healthy" {
            println!(
                "group={} status={} bad_shards={} provider_errors={}",
                group.group, group.status, group.bad_shards, group.provider_errors
            );
        }
    }
    for shard in &report.shards {
        if shard.status != "healthy" {
            println!(
                "bad_shard={} kind={:?} group={} slot={} remote={} status={} detail={}",
                shard.index,
                shard.kind,
                shard.group,
                shard.slot,
                shard.remote,
                shard.status,
                shard.detail.as_deref().unwrap_or("")
            );
        }
    }
}
