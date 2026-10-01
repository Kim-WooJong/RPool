//! `pool migrate retire`: report what a completed migration left behind
//! (dry run, the default), and with `--confirm` run the two cleanup steps:
//!
//! 1. **Delete** fossils whose grace elapsed, after a fresh check (reference
//!    re-check, replacement re-verification, unchanged objects), and resume
//!    deletions an interrupted run started.
//! 2. **Quarantine** new candidates: one `Fossil` record per item with the
//!    exact objects and the grace period. Nothing is moved or deleted.
//!
//! The mass-delete guard is checked for both steps before anything is
//! written. Every step is journaled in the cloud (who, when, which objects)
//! and is idempotent: rerunning continues where a run stopped.
use super::fossil::{fold, ItemFossil};
use super::guard;
use super::io::RetireIo;
use super::model::{
    per_account, FossilState, Item, KeepReason, Kept, RetireKind, RetireObject, RetireOptions,
    RetireRecord, RetireReport, RetireStep, DELETE_BATCH, RECORD_VERSION,
};
use super::plan::{select, Selection, World};
use crate::migration::model::{Plan, Record};
use crate::prelude::*;

/// What the fresh check says about a due fossil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Delete,
    /// Definitely keep: record `Cancelled` (released from quarantine).
    Cancel(String),
    /// Cannot decide now (provider error, unreadable reference): wait.
    Postpone(String),
    /// Nothing of it is left in the cloud.
    Gone,
}

fn postpones(reason: KeepReason) -> bool {
    matches!(
        reason,
        KeepReason::ReferencesUncertain
            | KeepReason::Unreachable
            | KeepReason::OriginalUnreadable
            | KeepReason::ReplacementUnverified
    )
}

/// Fresh verdict for a quarantined item.
pub(crate) fn verdict(fossil: &ItemFossil, selection: &Selection) -> Verdict {
    let id = &fossil.fossil.item;
    if let Some(item) = selection.candidates.iter().find(|i| &i.archive_id == id) {
        let approved: BTreeSet<&str> = fossil
            .fossil
            .objects
            .iter()
            .map(|o| o.address.as_str())
            .collect();
        return match item
            .objects
            .iter()
            .find(|o| !approved.contains(o.address.as_str()))
        {
            Some(new) => Verdict::Cancel(format!(
                "objects changed during the grace period ({} appeared)",
                new.address
            )),
            None => Verdict::Delete,
        };
    }
    if let Some(kept) = selection.kept.iter().find(|k| &k.archive_id == id) {
        let text = format!("{}: {}", keep_label(kept.reason), kept.detail);
        return if postpones(kept.reason) {
            Verdict::Postpone(text)
        } else {
            Verdict::Cancel(text)
        };
    }
    Verdict::Gone
}

pub(crate) fn keep_label(reason: KeepReason) -> &'static str {
    match reason {
        KeepReason::DriveArchive => "drive revision",
        KeepReason::Lost => "not replaced",
        KeepReason::ReplacementUnverified => "replacement not re-verified",
        KeepReason::OriginalChanged => "original changed",
        KeepReason::OriginalUnreadable => "original unreadable",
        KeepReason::ObjectOutsideArchive => "object outside its archive",
        KeepReason::Referenced => "still referenced",
        KeepReason::ReferencesUncertain => "references could not all be read",
        KeepReason::Unreachable => "account unreachable",
        KeepReason::VerifiedCopy => "verified copy",
        KeepReason::UnexpectedId => "unexpected id",
    }
}

/// Refuses unless every movable entry is switched or lost.
pub(crate) fn ensure_complete(plan: &Plan, records: &[Record]) -> Result<()> {
    let status = crate::migration::status::summarize(plan, records);
    if status.abandoned {
        bail!(
            "migration {} was abandoned; nothing is retired",
            plan.migration_id
        );
    }
    if !status.complete {
        bail!(
            "migration {} is not complete ({}/{} switched); finish it before cleaning up",
            plan.migration_id,
            status.switched,
            status.to_move
        );
    }
    Ok(())
}

fn pool_totals(world: &World, include_removed: bool) -> guard::Totals {
    let roots: Vec<&String> = world
        .listings
        .keys()
        .filter(|root| include_removed || world.pool_roots.contains(*root))
        .collect();
    guard::totals(&world.listings, roots)
}

/// The report for the current records (no side effects).
pub(crate) fn report(
    plan: &Plan,
    world: &World,
    selection: &Selection,
    fossils: &BTreeMap<String, ItemFossil>,
    options: &RetireOptions,
    now: u64,
) -> RetireReport {
    let active = |id: &str| fossils.get(id).is_some_and(|f| f.state(now).is_some());
    let candidates: Vec<Item> = selection
        .candidates
        .iter()
        .filter(|i| !active(&i.archive_id))
        .cloned()
        .collect();
    let kept: Vec<Kept> = selection
        .kept
        .iter()
        .filter(|k| !active(&k.archive_id))
        .cloned()
        .collect();
    let mut quarantine = vec![];
    let mut purged = 0;
    let mut due_objects: Vec<RetireObject> = vec![];
    for fossil in fossils.values() {
        let Some(mut view) = fossil.view(now) else {
            continue;
        };
        match view.state {
            FossilState::Purged => {
                purged += 1;
                continue;
            }
            FossilState::Deleting => {}
            FossilState::Waiting | FossilState::Due => {
                view.blocked = match verdict(fossil, selection) {
                    Verdict::Delete | Verdict::Gone => None,
                    Verdict::Cancel(why) | Verdict::Postpone(why) => Some(why),
                };
                if view.state == FossilState::Due && view.blocked.is_none() {
                    due_objects.extend(fossil.remaining());
                }
            }
        }
        quarantine.push(view);
    }
    let totals = pool_totals(world, options.include_removed);
    let guard_for = |objects: &[RetireObject]| {
        guard::check(
            objects,
            totals,
            options.max_delete_percent,
            options.max_delete_objects,
        )
    };
    let candidate_objects: Vec<RetireObject> =
        candidates.iter().flat_map(|i| i.objects.clone()).collect();
    let quarantine_objects: Vec<RetireObject> = quarantine
        .iter()
        .flat_map(|v| v.item.objects.clone())
        .collect();
    RetireReport {
        migration_id: plan.migration_id.clone(),
        pool: plan.pool.clone(),
        now_unix: now,
        dry_run: true,
        candidate_accounts: per_account(&candidate_objects),
        quarantine_accounts: per_account(&quarantine_objects),
        left_on_removed: per_account(&selection.left_on_removed),
        guard: super::model::GuardView {
            pool_objects: totals.objects,
            pool_bytes: totals.bytes,
            max_percent: options.max_delete_percent,
            max_objects: options.max_delete_objects,
            quarantine_refusal: guard_for(&candidate_objects),
            delete_refusal: guard_for(&due_objects),
        },
        uncertain: world.refs.uncertain.clone(),
        candidates,
        quarantine,
        purged,
        kept,
        actions: vec![],
    }
}

/// Plans (and with `options.confirm` performs) the cleanup.
pub(crate) fn retire(io: &dyn RetireIo, options: &RetireOptions) -> Result<RetireReport> {
    let plan = io.plan()?;
    let records = io.records()?;
    ensure_complete(&plan, &records)?;
    let world = io.observe(&plan, &records, options)?;
    let selection = select(&plan, &records, &world, options.include_removed);
    let now = io.now();
    let fossils = fold(&io.retire_records()?);
    let first = report(&plan, &world, &selection, &fossils, options, now);
    if !options.confirm {
        return Ok(first);
    }
    let delete = matches!(options.step, RetireStep::All | RetireStep::Delete);
    let quarantine = matches!(options.step, RetireStep::All | RetireStep::Quarantine);
    if !options.force {
        let refusals = [
            (delete, &first.guard.delete_refusal, "permanent deletion"),
            (quarantine, &first.guard.quarantine_refusal, "quarantine"),
        ];
        for (selected, refusal, what) in refusals {
            if let (true, Some(why)) = (selected, refusal) {
                bail!("mass-delete guard: {what} refused: {why}; nothing was changed. Check the numbers, then rerun with --force (or raise --max-delete-percent / --max-delete-objects)");
            }
        }
    }
    let mut actions = vec![];
    if delete {
        delete_step(io, &fossils, &selection, now, &mut actions)?;
    }
    if quarantine {
        for item in &first.candidates {
            let record = RetireRecord {
                version: RECORD_VERSION,
                kind: RetireKind::Fossil,
                item: item.archive_id.clone(),
                item_kind: item.kind,
                fossil_id: io.new_id()?,
                pc_id: io.pc_id(),
                ts_unix: io.now(),
                grace_seconds: options.grace_seconds,
                objects: ordered(item.objects.clone()),
                replacement: item.replacement.clone(),
                original_name: item.original_name.clone(),
                detail: None,
            };
            io.append(&record)?;
            let line = format!(
                "quarantined {} ({} objects, {}); deletable after unix {}",
                item.archive_id,
                item.objects.len(),
                crate::presentation::format_bytes(item.bytes()),
                record.ts_unix.saturating_add(record.grace_seconds)
            );
            io.say(&line);
            actions.push(line);
        }
    }
    let fossils = fold(&io.retire_records()?);
    let mut out = report(&plan, &world, &selection, &fossils, options, io.now());
    out.dry_run = false;
    out.actions = actions;
    Ok(out)
}

/// Manifest replicas first: once they are gone no PC sees a half-deleted
/// archive as an archive; the shards follow.
fn ordered(mut objects: Vec<RetireObject>) -> Vec<RetireObject> {
    objects.sort_by_key(|o| (!o.address.ends_with("/manifest.json"), o.address.clone()));
    objects
}

fn record(io: &dyn RetireIo, fossil: &RetireRecord, kind: RetireKind) -> Result<RetireRecord> {
    Ok(RetireRecord {
        version: RECORD_VERSION,
        kind,
        item: fossil.item.clone(),
        item_kind: fossil.item_kind,
        fossil_id: fossil.fossil_id.clone(),
        pc_id: io.pc_id(),
        ts_unix: io.now(),
        grace_seconds: 0,
        objects: vec![],
        replacement: fossil.replacement.clone(),
        original_name: fossil.original_name.clone(),
        detail: None,
    })
}

fn delete_step(
    io: &dyn RetireIo,
    fossils: &BTreeMap<String, ItemFossil>,
    selection: &Selection,
    now: u64,
    actions: &mut Vec<String>,
) -> Result<()> {
    for fossil in fossils.values() {
        match fossil.state(now) {
            Some(FossilState::Deleting) => {
                actions.push(finish(io, fossil, "resumed")?);
            }
            Some(FossilState::Due) => {
                if let Some(line) = delete_due(io, fossil, selection)? {
                    actions.push(line);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// One due fossil: fresh verdict, then the point of no return.
fn delete_due(
    io: &dyn RetireIo,
    fossil: &ItemFossil,
    selection: &Selection,
) -> Result<Option<String>> {
    let head = &fossil.fossil;
    match verdict(fossil, selection) {
        Verdict::Postpone(why) => {
            let line = format!("postponed {}: {why}", head.item);
            io.say(&line);
            return Ok(Some(line));
        }
        Verdict::Cancel(why) => {
            let mut cancel = record(io, head, RetireKind::Cancelled)?;
            cancel.detail = Some(why.clone());
            io.append(&cancel)?;
            let line = format!("kept {} (deletion cancelled): {why}", head.item);
            io.say(&line);
            return Ok(Some(line));
        }
        Verdict::Delete | Verdict::Gone => {}
    }
    // A restore from another PC may have arrived since this run started.
    let fresh = fold(&io.retire_records()?);
    let still = |f: Option<&ItemFossil>| {
        f.is_some_and(|f| f.fossil.fossil_id == head.fossil_id && !f.released())
    };
    if !still(fresh.get(&head.item)) {
        return Ok(Some(format!("skipped {}: restored meanwhile", head.item)));
    }
    io.append(&record(io, head, RetireKind::Deleting)?)?;
    let fresh = fold(&io.retire_records()?);
    if fresh.get(&head.item).is_some_and(|f| f.restored) {
        let mut cancel = record(io, head, RetireKind::Cancelled)?;
        cancel.detail = Some("restored while deletion was starting".into());
        io.append(&cancel)?;
        return Ok(Some(format!("skipped {}: restored meanwhile", head.item)));
    }
    finish(io, fossil, "deleted").map(Some)
}

/// Deletes the remaining objects (journaled per batch), then `Purged`.
fn finish(io: &dyn RetireIo, fossil: &ItemFossil, verb: &str) -> Result<String> {
    let head = &fossil.fossil;
    io.forget(&head.item)?;
    let remaining = ordered(fossil.remaining());
    let bytes: u64 = remaining.iter().map(|o| o.size).sum();
    for batch in remaining.chunks(DELETE_BATCH) {
        for object in batch {
            io.delete(&object.address)
                .with_context(|| format!("deleting {}; rerun to resume", object.address))?;
        }
        let mut done = record(io, head, RetireKind::Deleted)?;
        done.objects = batch.to_vec();
        io.append(&done)?;
    }
    let mut purged = record(io, head, RetireKind::Purged)?;
    purged.detail = Some(format!("{} objects", head.objects.len()));
    io.append(&purged)?;
    let line = format!(
        "{verb} {} ({} objects, {})",
        head.item,
        remaining.len(),
        crate::presentation::format_bytes(bytes)
    );
    io.say(&line);
    Ok(line)
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
