//! `rpool pool migrate retire|restore`: clean up after a completed
//! migration (dry run by default) and undo a quarantine.
use crate::cli::pool::RetireStepArg;
use crate::migration::retire::execute::keep_label;
use crate::migration::retire::model::{
    AccountBytes, FossilState, RetireOptions, RetireReport, RetireStep,
};
use crate::presentation::format_bytes;
use anyhow::Result;

pub(crate) fn step(arg: RetireStepArg) -> RetireStep {
    match arg {
        RetireStepArg::All => RetireStep::All,
        RetireStepArg::Quarantine => RetireStep::Quarantine,
        RetireStepArg::Delete => RetireStep::Delete,
    }
}

pub(crate) fn retire(
    rclone: &str,
    pool: &str,
    id: &str,
    options: &RetireOptions,
    json: bool,
) -> Result<()> {
    let report = crate::migration::retire::retire(rclone, pool, id, options)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

pub(crate) fn restore(
    rclone: &str,
    pool: &str,
    id: &str,
    items: &[String],
    all: bool,
) -> Result<()> {
    let restored = crate::migration::retire::restore(rclone, pool, id, items, all)?;
    println!("retire_restored={}", restored.len());
    Ok(())
}

fn accounts(title: &str, rows: &[AccountBytes]) {
    for row in rows {
        println!(
            "  {title} {}: {} objects, {}",
            row.root,
            row.objects,
            format_bytes(row.bytes)
        );
    }
}

fn print_report(r: &RetireReport) {
    println!(
        "migration_id={} pool={} mode={}",
        r.migration_id,
        r.pool,
        if r.dry_run { "dry-run" } else { "confirmed" }
    );
    let bytes = |items: &mut dyn Iterator<Item = u64>| format_bytes(items.sum());
    println!(
        "candidates={} ({}) quarantined={} purged={} kept={}",
        r.candidates.len(),
        bytes(&mut r.candidates.iter().map(|i| i.bytes())),
        r.quarantine.len(),
        r.purged,
        r.kept.len()
    );
    for item in &r.candidates {
        println!(
            "candidate {:?} {} '{}' objects={} bytes={}{}",
            item.kind,
            item.archive_id,
            item.original_name,
            item.objects.len(),
            item.bytes(),
            item.replacement
                .as_deref()
                .map(|n| format!(" replaced_by={n}"))
                .unwrap_or_default()
        );
    }
    accounts("frees", &r.candidate_accounts);
    for view in &r.quarantine {
        let state = match view.state {
            FossilState::Waiting => format!("waiting until unix {}", view.due_unix),
            FossilState::Due => "due (deleted by the next --confirm run)".into(),
            FossilState::Deleting => format!("deleting ({} objects left)", view.remaining),
            FossilState::Purged => "purged".into(),
        };
        println!(
            "quarantine {} '{}' objects={} bytes={} by={} {state}{}{}",
            view.item.archive_id,
            view.item.original_name,
            view.item.objects.len(),
            view.item.bytes(),
            view.by_pc,
            view.blocked
                .as_deref()
                .map(|b| format!(" blocked: {b}"))
                .unwrap_or_default(),
            if view.restore_refused {
                " (restore came after deletion started)"
            } else {
                ""
            }
        );
    }
    accounts("quarantined", &r.quarantine_accounts);
    for kept in &r.kept {
        println!(
            "kept {:?} {} '{}': {} ({})",
            kept.kind,
            kept.archive_id,
            kept.original_name,
            keep_label(kept.reason),
            kept.detail
        );
    }
    accounts("left on removed account", &r.left_on_removed);
    for what in &r.uncertain {
        println!("uncertain reference source (nothing is deleted): {what}");
    }
    let g = &r.guard;
    println!(
        "guard pool_objects={} pool_bytes={} max_percent={} max_objects={}",
        g.pool_objects, g.pool_bytes, g.max_percent, g.max_objects
    );
    if let Some(why) = &g.quarantine_refusal {
        println!("guard: quarantine needs --force: {why}");
    }
    if let Some(why) = &g.delete_refusal {
        println!("guard: deletion needs --force: {why}");
    }
    for action in &r.actions {
        println!("done: {action}");
    }
    if r.dry_run {
        println!(
            "next: rpool pool migrate retire {} --id {} --confirm",
            r.pool, r.migration_id
        );
    }
}
