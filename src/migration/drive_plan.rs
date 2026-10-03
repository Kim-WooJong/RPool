//! Drive planner (phase 3): the drive's visible files, read from the cloud
//! without a workspace, classified exactly like archives (same listings,
//! probe, losses and estimates). An unaffected file is kept as is: the new
//! generation references its existing archive (pool sync never deletes
//! payloads).
//!
//! Only what the cloud records is planned: writes still pending on some PC
//! (not yet uploaded and published) are not visible here.
use super::drive_model::{
    entry_key, epoch_for, record_bytes, units, DriveEntry, DriveFile, DrivePlan, BOOTSTRAP_BYTES,
    BOOTSTRAP_RECORDS,
};
use super::drive_source::DriveSource;
use super::enumerate::{Cloud, Loaded, RemoteListing};
use super::estimate::Transfer;
use super::model::{Action, Counts, Plan};
use super::plan::{list_all, plan_entry, Listed, PlanOptions};
use crate::prelude::*;

/// Plans the drive of `plan.pool` against `plan.target`, reusing the archive
/// planner's listings (`listed`, extended with remotes only drive files use).
/// `Ok(None)` when the pool has no drive, or it could not be read (a note is
/// added to `plan` then).
pub(crate) fn plan_drive(
    cloud: &dyn Cloud,
    source: &dyn DriveSource,
    plan: &mut Plan,
    listed: &mut Listed,
    options: &PlanOptions,
) -> Result<Option<DrivePlan>> {
    let generations = match source.generations() {
        Ok(found) => found,
        Err(error) => {
            plan.notes.push(format!(
                "the drive could not be checked ({error:#}); it is not part of this plan. Plan again when every account is reachable"
            ));
            return Ok(None);
        }
    };
    let Some(generation) = generations.first().cloned() else {
        return Ok(None);
    };
    let mut notes = Vec::new();
    if generations.len() > 1 {
        let others: Vec<String> = generations[1..].iter().map(|g| g.label()).collect();
        notes.push(format!(
            "only the current drive generation ({}) is migrated; older or other generations stay as they are: {}",
            generation.label(),
            others.join(", ")
        ));
    }
    let view = match source.view(&generation) {
        Ok(view) => view,
        Err(error) => {
            plan.notes.push(format!(
                "the drive ({}) could not be read ({error:#}); it is not part of this plan",
                generation.label()
            ));
            return Ok(None);
        }
    };
    let target = plan.target.clone();
    let target_set: BTreeSet<String> = target.remotes.iter().cloned().collect();
    // A small-file pack moves once for all its members (`units`).
    let units = units(&view.files);
    let packs = units
        .iter()
        .filter(|u| u.path.starts_with("(pack "))
        .count();
    extend_listings(cloud, &mut listed.listings, &units);
    let workers = if options.workers > 0 {
        options.workers
    } else {
        target.workers.max(1)
    };
    let classified = classify(
        cloud,
        &target,
        &target_set,
        &listed.listings,
        options,
        workers,
        &units,
    )?;

    let mut counts = Counts::default();
    let (mut download, mut upload, mut new_storage, mut record_total) = (0u64, 0u64, 0u64, 0u64);
    let add = |a: u64, b: u64| a.checked_add(b).context("transfer estimate overflow");
    let mut specs = listed.specs.clone();
    for (entry, transfer) in &classified {
        match entry.action {
            Action::Unaffected => counts.unaffected += 1,
            Action::Relocate => counts.relocate += 1,
            Action::Reencode => counts.reencode += 1,
            Action::Lost => counts.lost += 1,
            Action::Unknown => counts.unknown += 1,
        }
        download = add(download, entry.download_bytes)?;
        upload = add(upload, entry.upload_bytes)?;
        new_storage = add(new_storage, transfer.specs.iter().map(|s| s.size).sum())?;
        specs.push(transfer.specs.clone());
    }
    // Every visible file (pack members too) gets its own record.
    for file in &view.files {
        record_total = add(record_total, record_bytes(&file.path, &file.manifest))?;
    }
    if packs > 0 {
        notes.push(format!(
            "drive: {packs} small-file pack(s) move as one unit each; every file in a pack keeps its position in the new pack archive"
        ));
    }
    let quota_ok = cloud.quota_ok(&target, &specs);
    // A fresh PC streams large generations in pages (metadata checkpoints);
    // past the page size it is only slower, so this is a note, not a refusal.
    let bootstrap_ok = true;
    if view.files.len() > BOOTSTRAP_RECORDS || record_total > BOOTSTRAP_BYTES {
        notes.push(format!(
            "the new drive generation holds {} files ({}) of records; a fresh PC reads them in pages. Run `rpool pool compact` after adoption to checkpoint them",
            view.files.len(),
            crate::presentation::format_bytes(record_total)
        ));
    }
    if counts.lost > 0 {
        notes.push(format!(
            "{} drive file(s) are unrecoverable (some group has fewer than K readable shards); adoption leaves them out only with --accept-lost",
            counts.lost
        ));
    }
    if counts.unknown > 0 {
        notes.push(format!(
            "{} drive file(s) could not be checked (provider errors); adoption checks them again",
            counts.unknown
        ));
    }
    notes.push(format!(
        "drive: {} file(s) of {} are planned. They move to a new drive generation only on adoption (`pool migrate adopt`); until then every PC keeps using the current one. When the drive part starts, PCs on {} stop publishing (their changes stay local).",
        view.files.len(),
        generation.label(),
        generation.label()
    ));
    notes.push("drive: only files published to the cloud are planned. Writes still pending on a PC (not uploaded yet) are not; let that PC finish syncing first. Files changed after planning are caught up during adoption.".into());
    if counts.unaffected > 0 {
        notes.push(format!(
            "drive: {} unaffected file(s) are kept as is; the new generation references their existing archives.",
            counts.unaffected
        ));
    }
    notes.push(format!(
        "drive history: only the visible files (and conflict copies) are carried over; earlier versions stay readable in the previous generation ({}).",
        generation.label()
    ));
    let entries = classified.into_iter().map(|(entry, _)| entry).collect();
    Ok(Some(DrivePlan {
        version: 1,
        migration_id: plan.migration_id.clone(),
        pool: plan.pool.clone(),
        source: generation,
        epoch: epoch_for(&plan.migration_id),
        entries,
        counts,
        download_bytes: download,
        upload_bytes: upload,
        new_storage_bytes: new_storage,
        estimated_seconds: super::speed::estimate_seconds(
            download,
            upload,
            options.download_mib_s,
            options.upload_mib_s,
        ),
        quota_ok,
        bootstrap_ok,
        notes,
    }))
}

/// Lists the remotes drive manifests use that are not listed yet.
pub(super) fn extend_listings(
    cloud: &dyn Cloud,
    listings: &mut BTreeMap<String, RemoteListing>,
    files: &[DriveFile],
) {
    let extra: BTreeSet<String> = files
        .iter()
        .flat_map(|f| f.manifest.shards.iter().map(|s| s.remote.clone()))
        .filter(|r| !listings.contains_key(r))
        .collect();
    if !extra.is_empty() {
        let configured = cloud.configured();
        listings.extend(list_all(cloud, &extra, configured.as_ref()));
    }
}

/// Classifies drive files (planning, and catch-up during adoption).
#[allow(clippy::too_many_arguments)]
pub(super) fn classify(
    cloud: &dyn Cloud,
    target: &PoolDefinition,
    target_set: &BTreeSet<String>,
    listings: &BTreeMap<String, RemoteListing>,
    options: &PlanOptions,
    workers: usize,
    files: &[DriveFile],
) -> Result<Vec<(DriveEntry, Transfer)>> {
    files
        .par_iter()
        .map(|file| {
            let fingerprint = crate::manifest::manifest_fingerprint(&file.manifest)?;
            let loaded = Loaded {
                manifest: file.manifest.clone(),
                source: format!("drive:{}", file.path),
                fingerprint: fingerprint.clone(),
            };
            let (entry, transfer) = plan_entry(
                cloud, &loaded, target, target_set, listings, options, workers,
            )?;
            let mut detail = entry.detail;
            if entry.action == Action::Unaffected {
                detail = Some(join(
                    detail,
                    "kept: the new generation references this archive",
                ));
            }
            Ok((
                DriveEntry {
                    key: entry_key(&file.path, &file.revision),
                    path: file.path.clone(),
                    revision: file.revision.clone(),
                    hash: file.hash.clone(),
                    size: file.size,
                    source_archive_id: file.manifest.archive_id.clone(),
                    fingerprint,
                    action: entry.action,
                    download_bytes: entry.download_bytes,
                    upload_bytes: entry.upload_bytes,
                    losses: entry.losses,
                    detail,
                    moves: entry.moves,
                },
                transfer,
            ))
        })
        .collect()
}

/// Appends `note` to an existing entry detail (`"; "`-separated).
fn join(detail: Option<String>, note: &str) -> String {
    match detail {
        Some(detail) => format!("{detail}; {note}"),
        None => note.to_owned(),
    }
}

#[cfg(test)]
#[path = "drive_plan_tests.rs"]
mod tests;
