//! `rpool pool migrate ...`: plan, run/resume, status, lost files, abandon.
use crate::cli::pool::{MigrateArgs, MigrateCommands, ProbeMode};
use crate::migration::model::{LostFile, MigrationStatus, Plan};
use crate::migration::{execute, plan::PlanOptions, speed, status};
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
            json,
        } => plan(
            rclone,
            &pool,
            probe,
            download_mib_s,
            upload_mib_s,
            measure_speed,
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
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("no migrations recorded for pool {pool}");
            } else {
                for s in &list {
                    print_status(s);
                }
            }
            Ok(())
        }
        MigrateCommands::Lost { pool, id, json } => {
            let s = one_status(rclone, &pool, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s.lost)?);
            } else {
                print_lost(&s.lost);
            }
            Ok(())
        }
        MigrateCommands::Abandon { pool, id } => execute::abandon(rclone, &pool, &id),
    }
}

/// History label and target for `rpool history`.
pub(crate) fn history_label(args: &MigrateArgs) -> (String, Option<String>) {
    let (op, pool, id) = match &args.command {
        MigrateCommands::Plan { pool, .. } => ("plan", pool, None),
        MigrateCommands::Run { pool, id, .. } => ("run", pool, Some(id)),
        MigrateCommands::Status { pool, id, .. } => ("status", pool, id.as_ref()),
        MigrateCommands::Lost { pool, id, .. } => ("lost", pool, Some(id)),
        MigrateCommands::Abandon { pool, id } => ("abandon", pool, Some(id)),
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

fn plan(
    rclone: &str,
    pool: &str,
    probe: ProbeMode,
    mut download_mib_s: Option<f64>,
    mut upload_mib_s: Option<f64>,
    measure_speed: bool,
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
    };
    let plan = execute::create(rclone, pool, &options)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        print_plan(&plan);
    }
    Ok(())
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
    println!(
        "next: rpool pool migrate run {} --id {}",
        plan.pool, plan.migration_id
    );
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
