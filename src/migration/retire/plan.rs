//! Candidate selection (pure): from the frozen plan, the migration records
//! and a fresh observation of the cloud ([`World`]), decide what may be
//! deleted and why everything else is kept.
//!
//! - **Originals**: entries whose winning record is `Switched`, whose
//!   replacement passes a fresh re-verification, whose current manifest is
//!   still the one that was migrated, and whose objects all sit inside the
//!   archive's own folder. Objects are what the root listings show under
//!   `<root>/<archive_id>/` (manifest replicas and shards).
//! - **Orphans**: replacement ids this migration claimed but never verified
//!   or switched (interrupted, failed or lost attempts). A verified copy is
//!   never an orphan: another PC may already use it.
//! - Anything referenced by another manifest, the drive or another
//!   migration in progress is kept, and while any reference source is
//!   unreadable everything is kept.
use super::model::{Item, ItemKind, KeepReason, Kept, RetireObject};
use super::refs::References;
use crate::migration::enumerate::{is_drive_archive, RemoteListing};
use crate::migration::execute::{progress, Progress};
use crate::migration::model::{Action, Entry, Plan, Record, RecordState};
use crate::migration::probe::quick_states;
use crate::prelude::*;
use crate::utils::relative_remote_object;

/// What the cloud looks like right now (read by the live side, faked in tests).
#[derive(Debug, Clone, Default)]
pub(crate) struct World {
    /// Roots (accounts) of the pool: the migration's target and the saved pool.
    pub pool_roots: BTreeSet<String>,
    /// Recursive listing per root: every pool root, and the originals' old
    /// roots when removed accounts are included.
    pub listings: BTreeMap<String, RemoteListing>,
    /// Current manifest of each switched original: `Ok(None)` when no
    /// manifest exists anywhere any more.
    pub originals: BTreeMap<String, Result<Option<Manifest>, String>>,
    /// Replacement manifests by new archive id.
    pub replacements: BTreeMap<String, Result<Manifest, String>>,
    /// Full readback of replacements (only with `--full-verify`).
    pub full_checks: BTreeMap<String, Result<(), String>>,
    /// References from manifests, the drive and other migrations.
    pub refs: References,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Result of [`select`]: what may be quarantined now and what is kept.
pub(crate) struct Selection {
    /// Items that pass every check (originals and orphans).
    pub candidates: Vec<Item>,
    /// Items that stay, each with its reason.
    pub kept: Vec<Kept>,
    /// Originals' objects on accounts that left the pool, not deleted.
    pub left_on_removed: Vec<RetireObject>,
}

/// Ids this migration generates for replacements: `migrate-<24 hex>`.
pub(crate) fn generated_id(id: &str) -> bool {
    id.strip_prefix("migrate-").is_some_and(|hex| {
        hex.len() == 24
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Files under `<root>/<id>/` of every listed root in `roots`; `Err` names
/// the first root whose listing failed.
fn folder_objects<'a>(
    listings: &BTreeMap<String, RemoteListing>,
    roots: impl IntoIterator<Item = &'a String>,
    id: &str,
) -> Result<Vec<RetireObject>, String> {
    let prefix = format!("{id}/");
    let mut out = vec![];
    for root in roots {
        match listings.get(root) {
            Some(RemoteListing::Listed(files)) => {
                for (path, size) in files.range(prefix.clone()..) {
                    if !path.starts_with(&prefix) {
                        break;
                    }
                    out.push(RetireObject {
                        address: crate::utils::remote_join(root, path),
                        size: *size,
                        root: root.clone(),
                    });
                }
            }
            Some(RemoteListing::Failed(error)) => return Err(format!("{root}: {error}")),
            Some(RemoteListing::NotConfigured) => {
                return Err(format!("{root} is not configured on this PC"))
            }
            None => return Err(format!("{root} was not listed")),
        }
    }
    Ok(out)
}

/// Builds a [`Kept`] entry.
fn kept(entry_id: &str, kind: ItemKind, name: &str, reason: KeepReason, detail: String) -> Kept {
    Kept {
        archive_id: entry_id.into(),
        kind,
        original_name: name.into(),
        reason,
        detail,
    }
}

/// Reference verdict shared by originals and orphans.
fn reference_check(
    refs: &References,
    id: &str,
    objects: &[RetireObject],
) -> Option<(KeepReason, String)> {
    let referrers = refs.referrers(id, objects.iter().map(|o| o.address.as_str()));
    if !referrers.is_empty() {
        let list: Vec<String> = referrers.into_iter().take(3).collect();
        return Some((KeepReason::Referenced, list.join("; ")));
    }
    if !refs.uncertain.is_empty() {
        return Some((KeepReason::ReferencesUncertain, refs.uncertain.join("; ")));
    }
    None
}

/// Fresh re-verification of a switched replacement.
fn replacement_ok(world: &World, entry: &Entry, record: &Record) -> Result<String, String> {
    let id = record
        .new_archive_id
        .clone()
        .ok_or("switched record without a replacement id")?;
    let manifest = match world.replacements.get(&id) {
        Some(Ok(manifest)) => manifest,
        Some(Err(error)) => return Err(error.clone()),
        None => return Err("replacement was not checked".into()),
    };
    if manifest.archive_id != id || manifest.original_size != entry.size {
        return Err("replacement manifest identity or size differs".into());
    }
    let states = quick_states(manifest, &world.listings, &world.pool_roots);
    if let Some((shard, state)) = manifest
        .shards
        .iter()
        .zip(&states)
        .find(|(_, s)| !s.is_ok())
    {
        return Err(format!(
            "shard {} on {}: {state:?}",
            shard.index, shard.remote
        ));
    }
    if let Some(Err(error)) = world.full_checks.get(&id) {
        return Err(format!("full readback failed: {error}"));
    }
    Ok(id)
}

/// Checks one switched original (drive archive, replacement, unchanged
/// manifest, objects inside its folder, references) and pushes it to
/// `out.candidates`, `out.kept` or `out.left_on_removed`.
fn original(
    world: &World,
    entry: &Entry,
    record: &Record,
    include_removed: bool,
    out: &mut Selection,
) {
    let id = entry.archive_id.as_str();
    let name = entry.original_name.as_str();
    let keep = |reason, detail: String| kept(id, ItemKind::Original, name, reason, detail);
    if is_drive_archive(id) {
        out.kept.push(keep(
            KeepReason::DriveArchive,
            "drive revisions are never retired".into(),
        ));
        return;
    }
    let replacement = match replacement_ok(world, entry, record) {
        Ok(replacement) => replacement,
        Err(error) => {
            out.kept
                .push(keep(KeepReason::ReplacementUnverified, error));
            return;
        }
    };
    let manifest = match world.originals.get(id) {
        Some(Ok(manifest)) => manifest.as_ref(),
        Some(Err(error)) => {
            out.kept
                .push(keep(KeepReason::OriginalUnreadable, error.clone()));
            return;
        }
        None => {
            out.kept
                .push(keep(KeepReason::OriginalUnreadable, "not observed".into()));
            return;
        }
    };
    // Roots holding this archive: the pool, plus the old shard roots.
    let mut roots: BTreeSet<String> = world.pool_roots.clone();
    let mut left = vec![];
    if let Some(manifest) = manifest {
        if manifest.archive_id != id {
            out.kept.push(keep(
                KeepReason::OriginalChanged,
                "manifest names another archive".into(),
            ));
            return;
        }
        match crate::manifest::manifest_fingerprint(manifest) {
            Ok(fp) if fp == entry.fingerprint => {}
            Ok(_) => {
                out.kept.push(keep(
                    KeepReason::OriginalChanged,
                    "the manifest changed after it was migrated".into(),
                ));
                return;
            }
            Err(error) => {
                out.kept
                    .push(keep(KeepReason::OriginalUnreadable, format!("{error:#}")));
                return;
            }
        }
        let prefix = format!("{id}/");
        for shard in &manifest.shards {
            match relative_remote_object(&shard.remote, &shard.object) {
                Ok(path) if path.starts_with(&prefix) => {}
                _ => {
                    out.kept.push(keep(
                        KeepReason::ObjectOutsideArchive,
                        format!("{} is outside {id}/", shard.object),
                    ));
                    return;
                }
            }
            if world.pool_roots.contains(&shard.remote) {
                continue;
            }
            let listed = matches!(
                world.listings.get(&shard.remote),
                Some(RemoteListing::Listed(_))
            );
            if include_removed && listed {
                roots.insert(shard.remote.clone());
            } else {
                left.push(RetireObject {
                    address: shard.object.clone(),
                    size: shard.size,
                    root: shard.remote.clone(),
                });
            }
        }
    }
    let objects = match folder_objects(&world.listings, &roots, id) {
        Ok(objects) => objects,
        Err(error) => {
            out.kept.push(keep(KeepReason::Unreachable, error));
            return;
        }
    };
    if let Some((reason, detail)) = reference_check(&world.refs, id, &objects) {
        out.kept.push(keep(reason, detail));
        return;
    }
    out.left_on_removed.extend(left);
    if objects.is_empty() {
        return;
    }
    out.candidates.push(Item {
        archive_id: id.into(),
        kind: ItemKind::Original,
        original_name: name.into(),
        replacement: Some(replacement),
        objects,
    });
}

/// Replacement ids that only ever were claimed (or lost) by an attempt, with
/// the entry they belong to.
pub(crate) fn orphan_ids(records: &[Record]) -> BTreeMap<String, String> {
    let mut used = BTreeSet::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for record in records {
        let Some(id) = &record.new_archive_id else {
            continue;
        };
        if matches!(record.state, RecordState::Verified | RecordState::Switched) {
            used.insert(id.clone());
        } else {
            seen.entry(id.clone())
                .or_insert_with(|| record.entry.clone());
        }
    }
    seen.retain(|id, _| !used.contains(id));
    seen
}

/// Checks one orphan copy id (generated id, listable, not referenced) and
/// pushes it to `out.candidates` or `out.kept`; empty folders are skipped.
fn orphan(world: &World, id: &str, entry_name: &str, out: &mut Selection) {
    let name = format!("{entry_name} (partial copy)");
    let keep = |reason, detail: String| kept(id, ItemKind::Orphan, &name, reason, detail);
    if !generated_id(id) {
        out.kept.push(keep(
            KeepReason::UnexpectedId,
            "not a migration copy id".into(),
        ));
        return;
    }
    let objects = match folder_objects(&world.listings, &world.pool_roots, id) {
        Ok(objects) => objects,
        Err(error) => {
            out.kept.push(keep(KeepReason::Unreachable, error));
            return;
        }
    };
    if objects.is_empty() {
        return;
    }
    if let Some((reason, detail)) = reference_check(&world.refs, id, &objects) {
        out.kept.push(keep(reason, detail));
        return;
    }
    out.candidates.push(Item {
        archive_id: id.into(),
        kind: ItemKind::Orphan,
        original_name: name,
        replacement: None,
        objects,
    });
}

/// What may be cleaned up now. The caller has checked that the migration
/// is complete and not abandoned.
pub(crate) fn select(
    plan: &Plan,
    records: &[Record],
    world: &World,
    include_removed: bool,
) -> Selection {
    let mut out = Selection::default();
    let state = progress(records);
    let names: BTreeMap<&str, &str> = plan
        .entries
        .iter()
        .map(|e| (e.archive_id.as_str(), e.original_name.as_str()))
        .collect();
    for entry in &plan.entries {
        match (entry.action, state.get(&entry.archive_id)) {
            (Action::Relocate | Action::Reencode, Some(Progress::Switched(record))) => {
                original(world, entry, record, include_removed, &mut out)
            }
            (Action::Relocate | Action::Reencode | Action::Lost, _) => out.kept.push(kept(
                &entry.archive_id,
                ItemKind::Original,
                &entry.original_name,
                KeepReason::Lost,
                "not replaced; the original is kept".into(),
            )),
            _ => {}
        }
    }
    let replacements: BTreeSet<String> = state
        .values()
        .filter_map(|p| match p {
            Progress::Switched(r) | Progress::Verified(r) => r.new_archive_id.clone(),
            _ => None,
        })
        .collect();
    for (id, entry) in orphan_ids(records) {
        if replacements.contains(&id) {
            continue;
        }
        let name = names.get(entry.as_str()).copied().unwrap_or(entry.as_str());
        orphan(world, &id, name, &mut out);
    }
    out
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
