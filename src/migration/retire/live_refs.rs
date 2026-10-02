//! Live reference collection (read-only): every manifest of the local
//! inventory and every manifest replica on the listed accounts (except the
//! cleanup subjects' own), the drive's cloud metadata of every generation,
//! local drive workspaces given by the user, and other migrations of the
//! pool that are still in progress. A source that cannot be read is
//! recorded as uncertain, which keeps everything.
use super::refs::References;
use crate::migration::enumerate::{is_drive_archive, manifest_ids, RemoteListing};
use crate::migration::journal::{discover, Journal};
use crate::migration::model::Action;
use crate::prelude::*;
use crate::storage::reader::StorageReader;

/// Inputs of reference collection; built by `retire::observe` and the drive
/// history cleanup (`drive_history::cleanup::live`).
pub(crate) struct Sources<'a> {
    /// Path of the rclone binary.
    pub rclone: &'a str,
    /// Pool name.
    pub pool: &'a str,
    /// Pool definition (for the drive's cloud metadata).
    pub policy: &'a PoolDefinition,
    /// Current migration; excluded from `other_migrations`.
    pub migration_id: &'a str,
    /// Fresh listings per root; their manifest replicas are references.
    pub listings: &'a BTreeMap<String, RemoteListing>,
    /// Archives being cleaned up: their own manifests are not references.
    pub subjects: &'a BTreeSet<String>,
    /// Local drive workspaces whose JSON files are scanned for references.
    pub workspaces: &'a [PathBuf],
    /// Reader used to load manifests.
    pub reader: &'a StorageReader,
}

/// Collects references from all sources: inventory, cloud manifests, drive
/// metadata, given workspaces and other in-progress migrations.
pub(crate) fn collect(sources: &Sources<'_>) -> References {
    let mut refs = References::default();
    inventory(sources, &mut refs);
    cloud_manifests(sources, &mut refs);
    let drive = crate::mount::drive_references::collect(
        sources.rclone,
        sources.pool,
        sources.policy,
        &mut |source, value| refs.add_json(source, value),
    );
    if let Err(error) = drive {
        refs.uncertain(format!("drive metadata: {error:#}"));
    }
    // A partly read workspace only adds references; it still counts as uncertain.
    for workspace in sources.workspaces {
        let label = format!("drive workspace {}", workspace.display());
        let read = crate::mount::drive_references::json_tree(workspace, &label, 4, &mut |s, v| {
            refs.add_json(s, v)
        });
        if let Err(error) = read {
            refs.uncertain(format!("{label}: {error:#}"));
        }
    }
    other_migrations(sources, &mut refs);
    refs
}

/// Adds every inventory manifest (except subjects); an unreadable one marks
/// the result uncertain only when it lists one of the listed roots.
pub(crate) fn inventory(sources: &Sources<'_>, refs: &mut References) {
    let store = match crate::inventory::load_inventory() {
        Ok(store) => store,
        Err(error) => {
            refs.uncertain(format!("local inventory: {error:#}"));
            return;
        }
    };
    let roots: BTreeSet<&String> = sources.listings.keys().collect();
    let loaded: Vec<(String, Result<Manifest>, bool)> = store
        .entries
        .values()
        .filter(|e| !sources.subjects.contains(&e.archive_id))
        .collect::<Vec<_>>()
        .par_iter()
        .map(|entry| {
            let relevant = entry.remotes.iter().any(|r| roots.contains(r));
            (
                format!("inventory entry {}", entry.archive_id),
                crate::manifest::load_manifest_with_storage(sources.reader, &entry.manifest_source),
                relevant,
            )
        })
        .collect();
    for (source, manifest, relevant) in loaded {
        match manifest {
            Ok(manifest) => refs.add_manifest(&source, &manifest),
            // Only entries on these accounts could refer to their objects.
            Err(error) if relevant => refs.uncertain(format!("{source}: {error:#}")),
            Err(_) => {}
        }
    }
}

/// Adds every manifest replica found on the listed roots (except subjects and
/// drive archives), trying each replica of an id until one loads.
pub(crate) fn cloud_manifests(sources: &Sources<'_>, refs: &mut References) {
    let mut by_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, listing) in sources.listings {
        if let Some(files) = listing.files() {
            for (id, _) in manifest_ids(files) {
                if sources.subjects.contains(&id) || is_drive_archive(&id) {
                    continue;
                }
                by_id
                    .entry(id.clone())
                    .or_default()
                    .push(crate::utils::remote_join(
                        root,
                        &format!("{id}/manifest.json"),
                    ));
            }
        }
    }
    let loaded: Vec<(String, Result<Manifest, String>)> = by_id
        .into_par_iter()
        .map(|(id, addresses)| {
            let mut errors = vec![];
            for address in &addresses {
                match crate::manifest::load_manifest_with_storage(sources.reader, address) {
                    Ok(manifest) => return (id, Ok(manifest)),
                    Err(error) => errors.push(format!("{address}: {error:#}")),
                }
            }
            (id, Err(errors.join("; ")))
        })
        .collect();
    for (id, manifest) in loaded {
        match manifest {
            Ok(manifest) => refs.add_manifest(&format!("manifest of {id}"), &manifest),
            Err(error) => refs.uncertain(format!("manifest of {id}: {error}")),
        }
    }
}

/// Migrations still in progress may read their sources or be writing
/// copies: everything they name is referenced.
pub(crate) fn other_migrations(sources: &Sources<'_>, refs: &mut References) {
    let ids = match discover(sources.rclone, sources.pool) {
        Ok(ids) => ids,
        Err(error) => {
            refs.uncertain(format!("other migrations: {error:#}"));
            return;
        }
    };
    for id in ids.into_iter().filter(|id| id != sources.migration_id) {
        let label = format!("migration {} (in progress)", &id[..id.len().min(12)]);
        let loaded = Journal::open(sources.rclone, sources.pool, &id).and_then(|journal| {
            let plan = journal
                .load_plan()?
                .ok_or_else(|| anyhow!("no readable plan"))?;
            Ok((plan, journal.records()?))
        });
        let (plan, records) = match loaded {
            Ok(found) => found,
            Err(error) => {
                refs.uncertain(format!("migration {id}: {error:#}"));
                continue;
            }
        };
        let status = crate::migration::status::summarize(&plan, &records);
        if status.abandoned || status.complete {
            continue;
        }
        for entry in &plan.entries {
            if matches!(entry.action, Action::Relocate | Action::Reencode) {
                refs.add_text(&label, &entry.archive_id);
            }
        }
        for record in &records {
            if let Some(new) = &record.new_archive_id {
                refs.add_text(&label, new);
            }
        }
    }
}
