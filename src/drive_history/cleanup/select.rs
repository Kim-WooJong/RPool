//! Which drive archives no one needs any more (pure; fresh inputs per run).
//!
//! An archive is a top-level `virtual-*` folder on a pool account: a file
//! version's own archive (`virtual-<id>`, its manifest replica and shards)
//! or an incremental upload's group part (`virtual-<id>-g<n>`). Every drive
//! revision's manifest names the folders holding its shards (an incremental
//! version also names its base's folders).
//!
//! Kept: every folder named by a revision that is current, in a trash entry
//! that has neither expired nor been purged, or a kept previous version
//! (`retention::protected`, unknown times never expire), in **any** drive
//! generation, or by this PC's local drive state; and every folder or object
//! named by anything else (`World::refs`: inventory, other manifests,
//! migrations in progress, drive uploads no event names yet). Candidates:
//! folders named only by other revisions, with objects still listed.
use super::super::graph::History;
use super::super::model::Retention;
use crate::migration::enumerate::RemoteListing;
use crate::migration::retire::guard::Totals;
use crate::migration::retire::model::RetireObject;
use crate::migration::retire::refs::References;
use crate::prelude::*;

/// Fresh observation of everything that may reference drive data.
pub(crate) struct World {
    /// Every drive generation's history, and this PC's local drive state.
    pub histories: Vec<(String, History)>,
    /// Manifests this PC's open drive still needs (open files, bases of
    /// uploads not finished yet).
    pub local_kept: Vec<(String, Manifest)>,
    /// References from outside the drive's revisions.
    pub refs: References,
    /// Recursive listing of every pool account root.
    pub listings: BTreeMap<String, RemoteListing>,
    /// Sources that could not be read (postpones everything).
    pub uncertain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One unreferenced drive archive folder, as listed on the pool accounts.
pub(crate) struct Archive {
    /// Listed objects, manifest replicas first.
    pub objects: Vec<RetireObject>,
    /// A file version's own archive (not an incremental part).
    pub version: bool,
}
impl Archive {
    /// Total stored bytes of the listed objects.
    pub(crate) fn bytes(&self) -> u64 {
        self.objects.iter().map(|o| o.size).sum()
    }
}

#[derive(Debug, Default)]
/// Result of `select`: what may be deleted and what blocks a run.
pub(crate) struct Selection {
    /// Unreferenced archives with objects still stored.
    pub candidates: BTreeMap<String, Archive>,
    /// Named, unreferenced, but no object is stored any more.
    pub gone: BTreeSet<String>,
    /// Drive objects stored on the listed accounts (mass-delete guard).
    pub totals: Totals,
    /// Unreadable sources; a non-empty list postpones the run.
    pub uncertain: Vec<String>,
}

/// Top-level folder of a shard object under its account root.
fn folder(shard: &Shard) -> Option<String> {
    let rel = crate::utils::relative_remote_object(&shard.remote, &shard.object).ok()?;
    let first = rel.split('/').next()?;
    (!first.is_empty()).then(|| first.to_owned())
}

/// Folders a manifest names: its own archive and its shards' folders.
pub(crate) fn folders(manifest: &Manifest) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = manifest.shards.iter().filter_map(folder).collect();
    out.insert(manifest.archive_id.clone());
    out
}

/// Whether `name` is a drive archive folder (`virtual-*`).
pub(crate) fn is_drive_folder(name: &str) -> bool {
    crate::migration::enumerate::is_drive_archive(name)
}

/// A manifest that keeps its folders and objects. Its owner is not the
/// archive itself, so (unlike `References::add_manifest` for a cleanup
/// subject) it also keeps its own archive.
fn keep(refs: &mut References, source: &str, manifest: &Manifest) {
    let mut owned = manifest.clone();
    owned.archive_id = format!("kept:{source}");
    refs.add_manifest(source, &owned);
    refs.add_text(source, &manifest.archive_id);
}

/// Revisions whose data a generation keeps: current ones and what
/// retention protects.
pub(crate) fn kept_revisions(
    history: &History,
    retention: &Retention,
    now: u64,
) -> Result<BTreeSet<String>> {
    let clock = |id: &str| history.revs.get(id).and_then(|r| r.time);
    let mut keep = super::super::retention::protected(history, retention, now, &clock);
    keep.extend(history.view_at(None)?.into_values());
    Ok(keep)
}

/// Decide which drive archives are unreferenced: keep everything named by a
/// kept revision, this PC's drive or `World::refs`, then list the remaining
/// named folders as candidates (objects stored) or gone (nothing stored).
/// Pure; called by `execute::run` before marking and again before deleting.
pub(crate) fn select(world: &World, retention: &Retention, now: u64) -> Selection {
    let mut out = Selection {
        uncertain: world.uncertain.clone(),
        ..Default::default()
    };
    out.uncertain.extend(world.refs.uncertain.iter().cloned());
    let mut refs = world.refs.clone();
    // Folder -> it is a version's own archive.
    let mut named: BTreeMap<String, bool> = BTreeMap::new();
    for (label, history) in &world.histories {
        let kept = match kept_revisions(history, retention, now) {
            Ok(kept) => kept,
            Err(error) => {
                out.uncertain.push(format!("{label}: {error:#}"));
                continue;
            }
        };
        for (id, payload) in &history.payloads {
            let manifest = &payload.manifest;
            if kept.contains(id) {
                keep(&mut refs, &format!("{label}: revision {id}"), manifest);
                continue;
            }
            for name in folders(manifest) {
                let own = name == manifest.archive_id;
                *named.entry(name).or_insert(false) |= own;
            }
        }
    }
    for (label, manifest) in &world.local_kept {
        keep(&mut refs, label, manifest);
    }
    for (root, listing) in &world.listings {
        match listing {
            RemoteListing::Listed(files) => {
                for (path, size) in files {
                    if path.split('/').next().is_some_and(is_drive_folder) {
                        out.totals.objects += 1;
                        out.totals.bytes += size;
                    }
                }
            }
            RemoteListing::NotConfigured => out
                .uncertain
                .push(format!("{root}: not in this PC's rclone config")),
            RemoteListing::Failed(error) => out
                .uncertain
                .push(format!("{root}: listing failed: {error}")),
        }
    }
    for (name, version) in named {
        if !is_drive_folder(&name) {
            continue;
        }
        let objects = listed(&world.listings, &name);
        let addresses = objects.iter().map(|o| o.address.as_str());
        if !refs.referrers(&name, addresses).is_empty() {
            continue;
        }
        if objects.is_empty() {
            out.gone.insert(name);
        } else {
            out.candidates.insert(name, Archive { objects, version });
        }
    }
    out
}

/// Objects stored under `folder/` on every listed root, manifest replicas
/// first.
fn listed(listings: &BTreeMap<String, RemoteListing>, folder: &str) -> Vec<RetireObject> {
    let prefix = format!("{folder}/");
    let mut out = Vec::new();
    for (root, listing) in listings {
        let Some(files) = listing.files() else {
            continue;
        };
        for (path, size) in files.range(prefix.clone()..) {
            if !path.starts_with(&prefix) {
                break;
            }
            out.push(RetireObject {
                address: crate::utils::remote_join(root, path),
                size: *size,
                root: root.to_string(),
            });
        }
    }
    out.sort_by_key(|o| (!o.address.ends_with("/manifest.json"), o.address.clone()));
    out
}
