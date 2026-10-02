//! `rpool drive …` CLI: builds the [`Op`], runs it where it belongs
//! (`dispatch`) and prints text or the JSON contract.
use super::model::{
    CleanupMode, CleanupReport, CleanupTotals, RollbackPlan, TrashEntry, VersionEntry,
};
use super::ops::{Op, PurgeReport, RestoreReport};
use super::restore::Action;
use crate::cli::{
    DriveArgs, DriveCommands, DriveTarget, RetentionArgs, RetentionCommands, TrashArgs,
    TrashCommands, VersionsArgs, VersionsCommands,
};
use crate::prelude::*;

/// Operation name and target recorded in `rpool history`.
pub(crate) fn describe(args: &DriveArgs) -> (String, Option<String>) {
    let (name, pool) = match &args.command {
        DriveCommands::Trash(TrashArgs { command }) => match command {
            TrashCommands::List { target } => ("drive-trash-list", &target.pool),
            TrashCommands::Restore { target, .. } => ("drive-trash-restore", &target.pool),
            TrashCommands::Purge {
                target, confirm, ..
            } => (
                if *confirm {
                    "drive-trash-purge"
                } else {
                    "drive-trash-purge-preview"
                },
                &target.pool,
            ),
            TrashCommands::Empty { target, .. } => ("drive-trash-empty", &target.pool),
        },
        DriveCommands::Versions(VersionsArgs { command }) => match command {
            VersionsCommands::List { target, .. } => ("drive-versions-list", &target.pool),
            VersionsCommands::Restore { target, .. } => ("drive-versions-restore", &target.pool),
        },
        DriveCommands::Rollback {
            target, confirm, ..
        } => (
            if *confirm {
                "drive-rollback"
            } else {
                "drive-rollback-preview"
            },
            &target.pool,
        ),
        DriveCommands::Retention(RetentionArgs { command }) => match command {
            RetentionCommands::Show { pool, .. } => ("drive-retention-show", pool),
            RetentionCommands::Set { pool, .. } => ("drive-retention-set", pool),
        },
        DriveCommands::Cleanup {
            target,
            confirm,
            cancel,
            ..
        } => (
            if *cancel {
                "drive-cleanup-cancel"
            } else if *confirm {
                "drive-cleanup"
            } else {
                "drive-cleanup-preview"
            },
            &target.pool,
        ),
    };
    (name.into(), Some(pool.clone()))
}

/// The op and target of a drive command (`None`: retention, handled locally).
pub(crate) fn op(command: &DriveCommands, now: u64) -> Result<Option<(Op, DriveTarget)>> {
    Ok(Some(match command {
        DriveCommands::Trash(TrashArgs { command }) => match command {
            TrashCommands::List { target } => (Op::TrashList, target.clone()),
            TrashCommands::Restore {
                target,
                ids,
                to,
                into,
            } => (
                Op::TrashRestore {
                    ids: ids.clone(),
                    to: to.clone(),
                    into: into.clone(),
                },
                target.clone(),
            ),
            TrashCommands::Purge {
                target,
                ids,
                expired,
                confirm,
            } => (
                Op::TrashPurge {
                    ids: ids.clone(),
                    expired: *expired,
                    all: false,
                    confirm: *confirm,
                },
                target.clone(),
            ),
            TrashCommands::Empty { target, confirm } => (
                Op::TrashPurge {
                    ids: vec![],
                    expired: false,
                    all: true,
                    confirm: *confirm,
                },
                target.clone(),
            ),
        },
        DriveCommands::Versions(VersionsArgs { command }) => match command {
            VersionsCommands::List { target, path } => {
                (Op::VersionsList { path: path.clone() }, target.clone())
            }
            VersionsCommands::Restore {
                target,
                path,
                id,
                as_copy,
            } => (
                Op::VersionsRestore {
                    path: path.clone(),
                    id: id.clone(),
                    as_copy: *as_copy,
                },
                target.clone(),
            ),
        },
        DriveCommands::Rollback {
            target,
            path,
            at,
            confirm,
        } => (
            Op::Rollback {
                path: path.clone(),
                at: super::time_arg::parse(at, now)?,
                confirm: *confirm,
            },
            target.clone(),
        ),
        DriveCommands::Cleanup {
            target,
            confirm,
            force,
            cancel,
        } => (
            Op::Cleanup(super::cleanup::Request {
                confirm: *confirm,
                force: *force,
                cancel: *cancel,
            }),
            target.clone(),
        ),
        DriveCommands::Retention(_) => return Ok(None),
    }))
}

/// Entry point of `rpool drive …`: retention is handled locally; every other
/// command is dispatched (mount, workspace or cloud) and printed as text or
/// JSON (`--json`). Notes go to stderr.
pub(crate) fn run(rclone: &str, args: DriveArgs) -> Result<()> {
    if let DriveCommands::Retention(RetentionArgs { command }) = &args.command {
        return retention(command);
    }
    let (op, target) = op(&args.command, crate::utils::now_unix())?.context("drive command")?;
    let (value, notes) =
        super::dispatch::run(rclone, &target.pool, target.workspace.as_deref(), &op)?;
    for note in &notes {
        eprintln!("{note}");
    }
    if target.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    print_text(&op, value)
}

/// `rpool drive retention show|set`: update/load the pool's retention and
/// cleanup settings and print them.
fn retention(command: &RetentionCommands) -> Result<()> {
    let (value, json) = match command {
        RetentionCommands::Show { pool, json } => (super::retention::load(pool)?, *json),
        RetentionCommands::Set {
            pool,
            trash_days,
            keep_versions,
            version_days,
            auto_cleanup,
            cleanup_grace_days,
            json,
        } => {
            if auto_cleanup.is_some() || cleanup_grace_days.is_some() {
                super::retention::update_cleanup(pool, *auto_cleanup, *cleanup_grace_days)?;
            }
            (
                super::retention::update(pool, *trash_days, *keep_versions, *version_days)?,
                *json,
            )
        }
    };
    let pool = match command {
        RetentionCommands::Show { pool, .. } | RetentionCommands::Set { pool, .. } => pool,
    };
    let cleanup = super::retention::load_cleanup(pool)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let days = |n: u32| {
            if n == 0 {
                "unlimited".to_string()
            } else {
                format!("{n} days")
            }
        };
        println!("Trash: {}", days(value.trash_days));
        println!(
            "Previous versions per file: {}",
            if value.keep_versions == 0 {
                "unlimited".into()
            } else {
                value.keep_versions.to_string()
            }
        );
        println!("Previous versions kept for: {}", days(value.version_days));
        println!(
            "Automatic cleanup: {} (marked data is deleted after {})",
            if cleanup.auto { "on" } else { "off" },
            days(cleanup.grace_days)
        );
    }
    Ok(())
}

/// A unix time for display, or "unknown time".
fn when(time: Option<u64>) -> String {
    time.map_or_else(|| "unknown time".into(), super::time_arg::format)
}

/// Print a dispatched op's JSON result as human-readable text.
fn print_text(op: &Op, value: Value) -> Result<()> {
    let bytes = crate::presentation::format_bytes;
    match op {
        Op::TrashList => {
            let entries: Vec<TrashEntry> = serde_json::from_value(value)?;
            if entries.is_empty() {
                println!("The trash is empty.");
            }
            for e in entries {
                println!(
                    "{}{}  {}  {}  deleted {} by {}{}{}",
                    e.path,
                    if e.is_dir { "/" } else { "" },
                    bytes(e.size),
                    e.id,
                    when(e.deleted_unix),
                    e.deleted_by.as_deref().unwrap_or("?"),
                    e.expires_unix
                        .map(|t| format!(", leaves the trash {}", super::time_arg::format(t)))
                        .unwrap_or_default(),
                    if e.path_taken {
                        " (path now taken: restores as a copy)"
                    } else {
                        ""
                    }
                );
            }
        }
        Op::VersionsList { .. } => {
            let entries: Vec<VersionEntry> = serde_json::from_value(value)?;
            for v in entries {
                println!(
                    "{}  {:?}  {}  {}  by {}{}{}  {}",
                    when(v.time_unix),
                    v.kind,
                    bytes(v.size),
                    v.path,
                    v.author.as_deref().unwrap_or("?"),
                    if v.current { "  [current]" } else { "" },
                    if v.restorable || v.kind == super::model::VersionKind::Deleted {
                        ""
                    } else {
                        "  [data no longer available]"
                    },
                    v.id
                );
            }
        }
        Op::Rollback { .. } => {
            let plan: RollbackPlan = serde_json::from_value(value)?;
            println!(
                "Rollback of {} to {}:",
                plan.scope,
                super::time_arg::format(plan.at_unix)
            );
            if plan.changes.is_empty() {
                println!("  nothing differs");
            }
            for c in &plan.changes {
                println!("  {:?} {} ({})", c.action, c.path, bytes(c.size));
            }
            for (path, reason) in &plan.skipped {
                println!("  skipped {path}: {reason}");
            }
            if plan.applied {
                println!("Applied as new versions; files created later are in the trash.");
            } else {
                println!("Preview only: add --confirm to apply.");
            }
        }
        Op::TrashPurge { .. } => {
            let report: PurgeReport = serde_json::from_value(value)?;
            println!(
                "{} {} trash entr{}; {} would no longer be needed.",
                if report.applied {
                    "Purged"
                } else {
                    "Would purge"
                },
                report.ids.len(),
                if report.ids.len() == 1 { "y" } else { "ies" },
                bytes(report.eligible_bytes)
            );
            for note in &report.notes {
                println!("{note}");
            }
            if !report.applied {
                println!("Preview only: add --confirm to purge.");
            }
        }
        Op::Cleanup(_) => print_cleanup(&serde_json::from_value(value)?),
        Op::TrashRestore { .. } | Op::VersionsRestore { .. } => {
            let report: RestoreReport = serde_json::from_value(value)?;
            for change in &report.changes {
                if let Action::Put { path, from, .. } = change {
                    match from {
                        Some(from) if from != path => println!("Restored /{from} as /{path}"),
                        _ => println!("Restored /{path}"),
                    }
                }
            }
            for note in &report.notes {
                println!("{note}");
            }
            if !report.published {
                println!("Not yet published; the drive syncs it later.");
            }
        }
    }
    Ok(())
}

/// Print a cleanup report as text (totals, accounts, next deletion, guard).
fn print_cleanup(report: &CleanupReport) {
    let bytes = crate::presentation::format_bytes;
    let line = |label: &str, t: &CleanupTotals| {
        println!(
            "{label}: {} in {} archive(s), {} file version(s), {} object(s)",
            bytes(t.bytes),
            t.archives,
            t.files,
            t.objects
        );
    };
    match report.mode {
        CleanupMode::Cancelled => {
            println!("Dropped {} pending mark(s).", report.released);
        }
        CleanupMode::Postponed => {
            println!("Cleanup postponed: these references could not be read:");
            for source in &report.postponed {
                println!("  {source}");
            }
        }
        CleanupMode::Preview | CleanupMode::Applied => {
            let applied = report.mode == CleanupMode::Applied;
            line(
                if applied {
                    "Marked now"
                } else {
                    "Reclaimable, not marked yet"
                },
                &report.candidates,
            );
            line("Waiting for the grace period", &report.waiting);
            line(
                if applied {
                    "Due (deleted unless refused)"
                } else {
                    "Due for deletion"
                },
                &report.due,
            );
            if applied {
                line("Deleted", &report.deleted);
            }
            for account in &report.accounts {
                println!(
                    "  {}: {} ({} objects)",
                    account.account,
                    bytes(account.bytes),
                    account.objects
                );
            }
            if report.released > 0 {
                println!(
                    "Released {} mark(s): data is referenced again.",
                    report.released
                );
            }
        }
    }
    if let Some(next) = report.next_deletion_unix {
        println!("Next deletion: {}", super::time_arg::format(next));
    }
    if let Some(guard) = &report.guard {
        println!("Mass-delete guard: {guard}; add --force to delete anyway.");
    }
    for note in &report.notes {
        println!("{note}");
    }
    println!(
        "Automatic cleanup: {}, grace {} day(s).",
        if report.settings.auto { "on" } else { "off" },
        report.settings.grace_days
    );
    if report.mode == CleanupMode::Preview {
        println!("Preview only: add --confirm to mark and delete due data.");
    }
}
