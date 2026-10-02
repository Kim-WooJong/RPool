//! Live observation for the cleanup (read-only): one recursive listing per
//! account, the originals' current manifests, the replacements' manifests
//! (optionally a full readback), and the references ([`super::live_refs`]).
use super::live_refs;
use super::model::RetireOptions;
use super::plan::{orphan_ids, World};
use crate::migration::enumerate::{parse_lsjson, RemoteListing};
use crate::migration::execute::{progress, Progress};
use crate::migration::model::{Action, Entry, Plan, Record};
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::rclone::RcloneContext;
use crate::storage::reader::StorageReader;
use crate::storage::traits::OperationContext;

/// Replacement id, its manifest, and the optional full readback.
type Check = (String, Result<Manifest, String>, Option<Result<(), String>>);

/// rclone remote name of a root (`name:path` -> `name`).
fn remote_name(root: &str) -> &str {
    root.split_once(':').map_or(root, |(name, _)| name)
}

/// One listing per root, in parallel; unconfigured remotes are not called.
pub(crate) fn list_roots(
    rclone: &str,
    roots: &BTreeSet<String>,
) -> BTreeMap<String, RemoteListing> {
    let configured: Option<BTreeSet<String>> =
        RcloneAdmin::inherited(rclone).discover().ok().map(|names| {
            names
                .iter()
                .map(|n| n.trim_end_matches(':').to_owned())
                .collect()
        });
    let context = RcloneContext::inherited(rclone);
    roots
        .par_iter()
        .map(|root| {
            if configured
                .as_ref()
                .is_some_and(|names| !names.contains(remote_name(root)))
            {
                return (root.clone(), RemoteListing::NotConfigured);
            }
            let listing = match context.list_recursive(&OperationContext::none(), root) {
                Ok(bytes) => match parse_lsjson(&bytes) {
                    Ok(files) => RemoteListing::Listed(files),
                    Err(error) => RemoteListing::Failed(format!("{error:#}")),
                },
                Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {
                    RemoteListing::Listed(BTreeMap::new())
                }
                Err(error) => RemoteListing::Failed(error.to_string()),
            };
            (root.clone(), listing)
        })
        .collect()
}

/// Manifest replica addresses of `id` present in the listings.
pub(crate) fn replicas(listings: &BTreeMap<String, RemoteListing>, id: &str) -> Vec<String> {
    let rel = format!("{id}/manifest.json");
    listings
        .iter()
        .filter(|(_, l)| l.files().is_some_and(|files| files.contains_key(&rel)))
        .map(|(root, _)| crate::utils::remote_join(root, &rel))
        .collect()
}

/// The original's manifest now: one that differs from the migrated one
/// wins (so a change is noticed); `Ok(None)` when none exists any more.
fn current_original(
    reader: &StorageReader,
    listings: &BTreeMap<String, RemoteListing>,
    entry: &Entry,
) -> Result<Option<Manifest>, String> {
    let mut addresses = replicas(listings, &entry.archive_id);
    if Path::new(&entry.source).exists() && !addresses.contains(&entry.source) {
        addresses.insert(0, entry.source.clone());
    }
    let mut found: Option<Manifest> = None;
    let mut errors = vec![];
    for address in addresses {
        let parsed = crate::manifest::load_manifest_with_storage(reader, &address)
            .and_then(|m| Ok((crate::manifest::manifest_fingerprint(&m)?, m)));
        match parsed {
            Ok((fp, manifest)) if fp != entry.fingerprint => return Ok(Some(manifest)),
            Ok((_, manifest)) => {
                found.get_or_insert(manifest);
            }
            Err(error) => errors.push(format!("{address}: {error:#}")),
        }
    }
    match (found, errors.is_empty()) {
        (Some(manifest), true) => Ok(Some(manifest)),
        (None, true) => Ok(None),
        (_, false) => Err(errors.join("; ")),
    }
}

/// Builds the fresh [`World`] for one cleanup run: lists the pool's roots
/// (plus removed ones with `include_removed`), reads switched originals and
/// replacements (optionally full readback) and collects references. Called
/// via `LiveIo::observe`.
pub(crate) fn observe(
    rclone: &str,
    pool: &str,
    plan: &Plan,
    records: &[Record],
    options: &RetireOptions,
) -> Result<World> {
    let saved = crate::pool::load_pool_store()?.pools.get(pool).cloned();
    let mut pool_roots: BTreeSet<String> = plan.target.remotes.iter().cloned().collect();
    if let Some(saved) = &saved {
        pool_roots.extend(crate::remote_root::apply_remote_roots(
            saved.remotes.clone(),
        )?);
    }
    let mut listings = list_roots(rclone, &pool_roots);
    let reader = StorageReader::rclone(rclone);
    let state = progress(records);
    let switched: Vec<(&Entry, &Record)> = plan
        .entries
        .iter()
        .filter(|e| matches!(e.action, Action::Relocate | Action::Reencode))
        .filter_map(|e| match state.get(&e.archive_id) {
            Some(Progress::Switched(r)) => Some((e, r)),
            _ => None,
        })
        .collect();
    let originals: BTreeMap<String, Result<Option<Manifest>, String>> = switched
        .par_iter()
        .map(|(entry, _)| {
            (
                entry.archive_id.clone(),
                current_original(&reader, &listings, entry),
            )
        })
        .collect();
    if options.include_removed {
        let old: BTreeSet<String> = originals
            .values()
            .filter_map(|o| o.as_ref().ok().and_then(Option::as_ref))
            .flat_map(|m| m.shards.iter().map(|s| s.remote.clone()))
            .filter(|root| !pool_roots.contains(root))
            .collect();
        listings.extend(list_roots(rclone, &old));
    }
    let workers = plan.target.workers.max(1);
    let checks: Vec<Check> = switched
        .par_iter()
        .filter_map(|(_, record)| {
            Some((record.new_archive_id.clone()?, record.new_manifest.clone()?))
        })
        .map(|(id, location)| {
            let manifest = crate::manifest::load_manifest_with_storage(&reader, &location)
                .and_then(|m| {
                    crate::manifest::validate_manifest(&m)?;
                    Ok(m)
                })
                .map_err(|e| format!("{location}: {e:#}"));
            let full = (options.full_verify && manifest.is_ok()).then(|| {
                crate::commands::verify_with_storage(&reader, &location, true, workers)
                    .map_err(|e| format!("{e:#}"))
            });
            (id, manifest, full)
        })
        .collect();
    let mut replacements = BTreeMap::new();
    let mut full_checks = BTreeMap::new();
    for (id, manifest, full) in checks {
        if let Some(full) = full {
            full_checks.insert(id.clone(), full);
        }
        replacements.insert(id, manifest);
    }
    let mut subjects: BTreeSet<String> =
        switched.iter().map(|(e, _)| e.archive_id.clone()).collect();
    subjects.extend(orphan_ids(records).into_keys());
    let policy = saved.unwrap_or_else(|| plan.target.clone());
    let refs = live_refs::collect(&live_refs::Sources {
        rclone,
        pool,
        policy: &policy,
        migration_id: &plan.migration_id,
        listings: &listings,
        subjects: &subjects,
        workspaces: &options.workspaces,
        reader: &reader,
    });
    Ok(World {
        pool_roots,
        listings,
        originals,
        replacements,
        full_checks,
        refs,
    })
}
