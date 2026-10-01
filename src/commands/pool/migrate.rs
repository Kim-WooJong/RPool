//! `rpool pool migrate ...`: plan, run/resume, status, lost files, abandon,
//! adopt (drive), retire and restore (cleanup).
use super::migrate_retire;
use crate::cli::pool::{MigrateArgs, MigrateCommands, ProbeMode};
use crate::migration::drive_model::{DrivePlan, DriveStatus};
use crate::migration::model::{LostFile, MigrationStatus, Plan};
use crate::migration::retire::model::RetireOptions;
use crate::migration::{drive_adopt, drive_status, execute, plan::PlanOptions, speed, status};
use crate::presentation::format_bytes;
use anyhow::{anyhow, bail, Result};

/// Sample size of the optional speed benchmark.
const SPEED_SAMPLE_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn run(rclone: &str, args: MigrateArgs) -> Result<()> {
    match args.command {
        MigrateCommands::Plan {
            pool,
            probe,
            download_mib_s,
            upload_mib_s,
            measure_speed,
            no_drive,
            json,
        } => plan(
            rclone,
            &pool,
            probe,
            download_mib_s,
            upload_mib_s,
            measure_speed,
            no_drive,
            json,
        ),
        MigrateCommands::Run {
            pool,
            id,
            stop_file,
            take_over,
            parallel,
        } => execute::run(
            rclone,
            &pool,
            &id,
            &execute::RunOptions {
                stop_file,
                take_over,
                parallel,
            },
        ),
        MigrateCommands::Status { pool, id, json } => {
            let list = status::status(rclone, &pool, id.as_deref())?;
            let rows = list
                .iter()
                .map(|s| {
                    Ok(StatusRow {
                        drive: drive_status::status(rclone, &pool, &s.migration_id)?,
                        status: s,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("no migrations recorded for pool {pool}");
            } else {
                for row in &rows {
                    print_status(row.status);
                    if let Some(drive) = &row.drive {
                        print_drive_status(&pool, drive);
                    }
                }
            }
            Ok(())
        }
        MigrateCommands::Lost { pool, id, json } => {
            let s = one_status(rclone, &pool, &id)?;
            let drive = drive_status::status(rclone, &pool, &id)?;
            let mut lost = s.lost.clone();
            if let Some(drive) = &drive {
                lost.extend(drive.lost.iter().cloned());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&lost)?);
            } else {
                print_lost(&s.lost);
                if let Some(drive) = &drive {
                    if !drive.lost.is_empty() {
                        println!("drive files (path, size, payload archive):");
                        print_lost(&drive.lost);
                    }
                }
            }
            Ok(())
        }
        MigrateCommands::Abandon { pool, id } => execute::abandon(rclone, &pool, &id),
        MigrateCommands::Retire {
            pool,
            id,
            dry_run: _,
            confirm,
            step,
            grace_days,
            include_removed_accounts,
            full_verify,
            max_delete_percent,
            max_delete_objects,
            force,
            workspaces,
            json,
        } => {
            let options = RetireOptions {
                confirm,
                step: migrate_retire::step(step),
                grace_seconds: grace_days * 86_400,
                include_removed: include_removed_accounts,
                full_verify,
                max_delete_percent,
                max_delete_objects,
                force,
                workspaces,
            };
            migrate_retire::retire(rclone, &pool, &id, &options, json)
        }
        MigrateCommands::Restore {
            pool,
            id,
            items,
            all,
        } => migrate_retire::restore(rclone, &pool, &id, &items, all),
        MigrateCommands::Adopt {
            pool,
            id,
            accept_lost,
            workspace,
            stop_file,
            take_over,
            parallel,
        } => adopt(
            rclone,
            &pool,
            &id,
            accept_lost,
            workspace.as_deref(),
            &execute::RunOptions {
                stop_file,
                take_over,
                parallel,
            },
        ),
    }
}

/// `status --json` row: the archive status with the drive part beside it.
#[derive(serde::Serialize)]
struct StatusRow<'a> {
    #[serde(flatten)]
    status: &'a MigrationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    drive: Option<DriveStatus>,
}

/// `plan --json`: the plan with its drive part beside it.
#[derive(serde::Serialize)]
struct PlanJson<'a> {
    #[serde(flatten)]
    plan: &'a Plan,
    #[serde(skip_serializing_if = "Option::is_none")]
    drive: Option<&'a DrivePlan>,
}

fn adopt(
    rclone: &str,
    pool: &str,
    id: &str,
    accept_lost: bool,
    workspace: Option<&std::path::Path>,
    options: &execute::RunOptions,
) -> Result<()> {
    let adoption = drive_adopt::adopt(rclone, pool, id, accept_lost, options)?;
    if !adoption.dropped.is_empty() {
        println!(
            "drive_dropped={} (unrecoverable; see `rpool pool migrate lost {pool} --id {id}`)",
            adoption.dropped.len()
        );
    }
    let Some(workspace) = workspace else {
        println!("next: every PC's new drive workspace opens the adopted drive; existing workspaces on the previous generation are refused until switched with `rpool pool migrate adopt {pool} --id {id} --workspace <path>`");
        return Ok(());
    };
    match crate::mount::adoption_workspace::switch(rclone, workspace, &adoption)? {
        None => println!(
            "workspace_switched=false ({} needs no switch; its next mount opens the adopted drive)",
            workspace.display()
        ),
        Some(switched) => {
            println!(
                "workspace_switched=true backup={}",
                switched.backup.display()
            );
            for change in &switched.local_only {
                println!("local_only: {change}");
            }
            for file in &switched.exported {
                println!("exported: {}", file.display());
            }
            if !switched.local_only.is_empty() {
                println!("These changes were only on this PC and are not in the adopted drive. The exported files (with their .json receipts naming the drive path) can be copied into the drive after mounting it.");
            }
            println!(
                "next: mount {} again; it opens the adopted drive",
                workspace.display()
            );
        }
    }
    Ok(())
}

/// History label and target for `rpool history`.
pub(crate) fn history_label(args: &MigrateArgs) -> (String, Option<String>) {
    let (op, pool, id) = match &args.command {
        MigrateCommands::Plan { pool, .. } => ("plan", pool, None),
        MigrateCommands::Run { pool, id, .. } => ("run", pool, Some(id)),
        MigrateCommands::Status { pool, id, .. } => ("status", pool, id.as_ref()),
        MigrateCommands::Lost { pool, id, .. } => ("lost", pool, Some(id)),
        MigrateCommands::Abandon { pool, id } => ("abandon", pool, Some(id)),
        MigrateCommands::Retire { pool, id, .. } => ("retire", pool, Some(id)),
        MigrateCommands::Restore { pool, id, .. } => ("restore", pool, Some(id)),
        MigrateCommands::Adopt { pool, id, .. } => ("adopt", pool, Some(id)),
    };
    let target = match id {
        Some(id) => format!("{pool}/{id}"),
        None => pool.clone(),
    };
    (format!("pool-migrate-{op}"), Some(target))
}

fn one_status(rclone: &str, pool: &str, id: &str) -> Result<MigrationStatus> {
    status::status(rclone, pool, Some(id))?
        .into_iter()
        .find(|s| s.migration_id == id)
        .ok_or_else(|| anyhow!("migration not found in the cloud: {id}"))
}

#[allow(clippy::too_many_arguments)]
fn plan(
    rclone: &str,
    pool: &str,
    probe: ProbeMode,
    mut download_mib_s: Option<f64>,
    mut upload_mib_s: Option<f64>,
    measure_speed: bool,
    no_drive: bool,
    json: bool,
) -> Result<()> {
    let definition = crate::pool::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .ok_or_else(|| anyhow!("pool not found: {pool}"))?;
    for rate in [download_mib_s, upload_mib_s].into_iter().flatten() {
        if !rate.is_finite() || rate <= 0.0 {
            bail!("bandwidth must be a finite positive MiB/s rate");
        }
    }
    let workers = definition.workers.max(1);
    if measure_speed {
        let remotes = crate::remote_root::apply_remote_roots(definition.remotes.clone())?;
        let report = speed::measure(rclone, &remotes, SPEED_SAMPLE_BYTES, workers)?;
        for r in &report.remotes {
            eprintln!(
                "speed remote={} download_mib_s={} upload_mib_s={}{}",
                r.remote,
                fmt_rate(r.download_mib_s),
                fmt_rate(r.upload_mib_s),
                r.error
                    .as_deref()
                    .map(|e| format!(" error={e}"))
                    .unwrap_or_default()
            );
        }
        download_mib_s = report.download_mib_s;
        upload_mib_s = report.upload_mib_s;
    }
    let options = PlanOptions {
        probe_full: probe == ProbeMode::Full,
        download_mib_s,
        upload_mib_s,
        workers,
        skip_drive: no_drive,
    };
    let (plan, drive) = execute::create_with_drive(rclone, pool, &options)?;
    if json {
        let out = PlanJson {
            plan: &plan,
            drive: drive.as_ref(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_plan(&plan);
        if let Some(drive) = &drive {
            print_drive_plan(drive);
        }
        println!(
            "next: rpool pool migrate run {} --id {}",
            plan.pool, plan.migration_id
        );
        if drive.is_some() {
            println!(
                "then: rpool pool migrate adopt {} --id {} (switches the drive)",
                plan.pool, plan.migration_id
            );
        }
    }
    Ok(())
}

fn print_drive_plan(drive: &DrivePlan) {
    let c = &drive.counts;
    println!(
        "drive {} -> epoch {} files={} unaffected={} relocate={} reencode={} lost={} unknown={}",
        drive.source.label(),
        &drive.epoch[..12],
        drive.entries.len(),
        c.unaffected,
        c.relocate,
        c.reencode,
        c.lost,
        c.unknown
    );
    println!(
        "drive transfer download={} upload={} new_storage={}",
        format_bytes(drive.download_bytes),
        format_bytes(drive.upload_bytes),
        format_bytes(drive.new_storage_bytes)
    );
    match drive.estimated_seconds {
        Some((fast, slow)) => println!(
            "drive eta {} (eta_seconds={fast:.0}..{slow:.0})",
            crate::migration::speed::format_duration_range(fast, slow)
        ),
        None => println!("drive eta unknown"),
    }
    println!(
        "drive quota (archives and drive) {}",
        match drive.quota_ok {
            Some(true) => "fits",
            Some(false) => "DOES NOT FIT: free space on the new policy is insufficient",
            None => "unknown",
        }
    );
    if !drive.bootstrap_ok {
        println!("drive bootstrap DOES NOT FIT: adoption will be refused");
    }
    for note in &drive.notes {
        println!("note: {note}");
    }
}

fn print_drive_status(pool: &str, d: &DriveStatus) {
    let state = match (&d.adopted, d.ready, d.frozen) {
        (Some(_), _, _) => "adopted",
        (None, true, _) => "ready-to-adopt",
        (None, false, true) => "in-progress (frozen)",
        (None, false, false) => "not-started",
    };
    println!(
        "  drive {} -> epoch {} state={state} to_move={} switched={} verified={} unknown={} lost={}",
        d.source.label(),
        &d.epoch[..12],
        d.to_move,
        d.switched,
        d.verified,
        d.failed_unknown,
        d.lost.len()
    );
    match &d.adopted {
        Some(a) => println!(
            "  drive adopted by {} at {} files={} dropped={}",
            a.pc_id,
            a.ts_unix,
            a.files,
            a.dropped.len()
        ),
        None if d.ready => println!(
            "  next: rpool pool migrate adopt {pool} --id {}",
            d.migration_id
        ),
        None => {}
    }
}

fn fmt_rate(rate: Option<f64>) -> String {
    rate.map_or_else(|| "unknown".into(), |r| format!("{r:.1}"))
}

fn print_plan(plan: &Plan) {
    let c = &plan.counts;
    println!("migration_id={}", plan.migration_id);
    println!(
        "archives unaffected={} relocate={} reencode={} lost={} unknown={}",
        c.unaffected, c.relocate, c.reencode, c.lost, c.unknown
    );
    println!(
        "transfer download={} upload={} new_storage={}",
        format_bytes(plan.download_bytes),
        format_bytes(plan.upload_bytes),
        format_bytes(plan.new_storage_bytes)
    );
    match plan.estimated_seconds {
        Some((fast, slow)) => println!(
            "eta {} (eta_seconds={fast:.0}..{slow:.0}) at download={} upload={} MiB/s",
            crate::migration::speed::format_duration_range(fast, slow),
            fmt_rate(plan.download_mib_s),
            fmt_rate(plan.upload_mib_s)
        ),
        None => println!("eta unknown (give --download-mib-s/--upload-mib-s or --measure-speed)"),
    }
    println!(
        "quota {}",
        match plan.quota_ok {
            Some(true) => "fits",
            Some(false) => "DOES NOT FIT: free space on the new policy is insufficient",
            None => "unknown",
        }
    );
    for note in &plan.notes {
        println!("note: {note}");
    }
    if c.lost > 0 {
        println!(
            "lost={} (see `rpool pool migrate lost {} --id {}`)",
            c.lost, plan.pool, plan.migration_id
        );
    }
}

fn print_status(s: &MigrationStatus) {
    println!(
        "migration_id={} pool={} created_unix={} created_by={} state={}",
        s.migration_id,
        s.pool,
        s.created_unix,
        s.created_by,
        if s.abandoned {
            "abandoned"
        } else if s.complete {
            "complete"
        } else {
            "in-progress"
        }
    );
    println!(
        "  to_move={} switched={} verified={} unknown={} lost={} last_activity_unix={} pcs={}",
        s.to_move,
        s.switched,
        s.verified,
        s.failed_unknown,
        s.lost.len(),
        s.last_activity_unix,
        s.pcs.join(",")
    );
}

fn print_lost(lost: &[LostFile]) {
    if lost.is_empty() {
        println!("no unrecoverable files");
        return;
    }
    for file in lost {
        println!(
            "{}\t{}\tarchive={}\tdetected={}",
            file.original_name,
            format_bytes(file.size),
            file.archive_id,
            file.detected
        );
        for g in &file.groups {
            println!(
                "  group {}: available {}/{}",
                g.group, g.available, g.required_k
            );
            for m in &g.missing {
                let reason = serde_json::to_value(m.reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{:?}", m.reason));
                println!("    shard {} on {}: {reason}", m.index, m.remote);
            }
        }
    }
    eprintln!("lost_files={}", lost.len());
}
