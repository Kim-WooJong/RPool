use crate::maintenance::{
    repair_manifest_with_storage, save_integrity_snapshot, scan_manifest_with_storage,
};
use crate::manifest::{load_manifest_with_storage, validate_manifest};
use crate::prelude::*;

pub(crate) fn repair(
    rclone: &str,
    manifest_src: &str,
    quick: bool,
    workers: usize,
    retries: u32,
    dry_run: bool,
    groups: Vec<u32>,
) -> Result<()> {
    repair_with_storage(
        &crate::storage::writer::StorageWriter::rclone(rclone),
        manifest_src,
        quick,
        workers,
        retries,
        dry_run,
        groups,
    )
}

pub(crate) fn repair_with_storage(
    storage: &crate::storage::writer::StorageWriter,
    manifest_src: &str,
    quick: bool,
    workers: usize,
    retries: u32,
    dry_run: bool,
    groups: Vec<u32>,
) -> Result<()> {
    let manifest = load_manifest_with_storage(storage.reader(), manifest_src)?;
    validate_manifest(&manifest)?;

    if quick {
        let (quick_report, _) =
            scan_manifest_with_storage(storage.reader(), &manifest, false, workers)?;
        if !dry_run {
            if let Err(error) = save_integrity_snapshot(manifest_src, &quick_report) {
                eprintln!("[integrity] failed to save quick repair snapshot: {error:#}");
            }
        }
        println!("preflight=quick complete; running mandatory full BLAKE3 scan before repair");
    }

    let (report, probes) = scan_manifest_with_storage(storage.reader(), &manifest, true, workers)?;
    if !dry_run {
        if let Err(error) = save_integrity_snapshot(manifest_src, &report) {
            eprintln!("[integrity] failed to save repair preflight snapshot: {error:#}");
        }
    }

    let selected: BTreeSet<u32> = groups.into_iter().collect();
    let repaired = if selected.is_empty() {
        if report.unrecoverable_groups > 0 {
            bail!("{} group(s) are unrecoverable", report.unrecoverable_groups);
        }
        repair_manifest_with_storage(storage, &manifest, &probes, retries, dry_run, None)?
    } else {
        validate_selected_groups(&report, &selected)?;
        repair_manifest_with_storage(
            storage,
            &manifest,
            &probes,
            retries,
            dry_run,
            Some(&selected),
        )?
    };

    println!("repair_candidates={repaired}");
    if repaired == 0 {
        println!("status=healthy");
    } else if dry_run {
        println!("status=dry-run");
    } else {
        let (after, _) = scan_manifest_with_storage(storage.reader(), &manifest, true, workers)?;
        if let Err(error) = save_integrity_snapshot(manifest_src, &after) {
            eprintln!("[integrity] failed to save post-repair snapshot: {error:#}");
        }
        if selected.is_empty() {
            if after.healthy != after.total {
                bail!(
                    "post-repair verification failed: {} bad shard(s) remain",
                    after.total - after.healthy
                );
            }
        } else {
            for group in &selected {
                let Some(status) = after
                    .groups
                    .iter()
                    .find(|candidate| candidate.group == *group)
                else {
                    bail!("post-repair verification cannot find group {group}");
                };
                if status.status != "healthy" {
                    bail!(
                        "post-repair verification failed for group {group}: status={}",
                        status.status
                    );
                }
            }
        }
        println!("status=repaired");
    }
    Ok(())
}

fn validate_selected_groups(report: &ScrubReport, selected: &BTreeSet<u32>) -> Result<()> {
    for group in selected {
        let status = report
            .groups
            .iter()
            .find(|candidate| candidate.group == *group)
            .ok_or_else(|| anyhow!("group {group} is not present in this archive"))?;
        match status.status.as_str() {
            "recoverable" => {}
            "provider-error" => bail!(
                "group {group} has provider/transport errors; resolve provider health before repair"
            ),
            "unrecoverable" => bail!("group {group} is unrecoverable"),
            "healthy" => bail!("group {group} is already healthy"),
            other => bail!("group {group} has unsupported integrity status: {other}"),
        }
    }
    Ok(())
}
