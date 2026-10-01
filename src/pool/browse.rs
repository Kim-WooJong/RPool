//! Read-only listing of a pool's drive (the virtual drive namespace) straight
//! from the pool-sync metadata in the cloud: folder and file names with
//! sizes, no workspace needed and nothing written anywhere.
//!
//! CONTRACT (shared by the backend and the GUI Library): keep these types
//! and the `browse` signature stable.

/// One entry of the drive, `/`-separated path relative to the drive root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BrowseEntry {
    pub path: String,
    /// Plaintext size in bytes; 0 for directories.
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PoolBrowse {
    pub pool: String,
    /// "v6" or "none" (the pool has no pool-sync drive yet).
    pub mode: String,
    /// Sorted by path; directories are listed explicitly (also implied ones).
    pub entries: Vec<BrowseEntry>,
    /// Conflict copies and other notes worth showing, human readable.
    pub notes: Vec<String>,
}

/// Lists the drive of `pool` from its cloud metadata. Read-only. Blocking:
/// call it off the UI thread.
///
/// v6 events are collected and projected directly without any workspace.
pub(crate) fn browse(rclone: &str, pool: &str) -> anyhow::Result<PoolBrowse> {
    use anyhow::Context;
    let policy = super::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .with_context(|| format!("pool not found: {pool}"))?;
    super::validate_pool(&policy)?;
    let mut generations = super::browse_generations::discover(rclone, pool, &policy.remotes)?;
    // A pool migration may have adopted the drive into a newer generation
    // (and superseded the one it came from).
    let mut migration_note = None;
    match crate::migration::drive_journal::known(rclone, pool) {
        Ok(known) => {
            generations = crate::migration::drive_generations::effective(generations, &known)
        }
        Err(error) => {
            migration_note = Some(format!(
                "pool migrations could not be checked ({error:#}); the newest generation is shown"
            ))
        }
    }
    // Newest generation first; fall back to older ones only if it cannot be
    // interpreted as a drive (for example only name records).
    for (index, generation) in generations.iter().enumerate() {
        let epoch = generation.epoch.as_deref();
        // Includes checkpointed events (deleted from the event folder).
        let events = crate::mount::metadata_pool::read_v6(rclone, pool, &policy, epoch)?;
        if let Some((files, conflicts)) = project_events(events)? {
            let mut result = listing(pool, "v6", files, &conflicts);
            if let Some(epoch) = epoch {
                result.notes.insert(
                    0,
                    format!(
                        "metadata generation {} (after Apply pool changes or a pool migration)",
                        &epoch[..epoch.len().min(12)]
                    ),
                );
            }
            let older = generations.len() - index - 1;
            if older > 0 {
                result.notes.insert(
                    usize::from(epoch.is_some()),
                    format!("{older} older metadata generation(s) are not shown"),
                );
            }
            result.notes.extend(migration_note);
            return Ok(result);
        }
    }
    Ok(PoolBrowse {
        pool: pool.into(),
        mode: "none".into(),
        ..Default::default()
    })
}

type Files = std::collections::BTreeMap<String, u64>;
type Conflicts = Vec<crate::mount::peer_projection::Conflict>;

/// `None` when no v6 events exist in any replica.
#[cfg(test)]
fn project_v6(
    stores: &[&dyn crate::mount::pool_sync::EventStore],
) -> anyhow::Result<Option<(Files, Conflicts)>> {
    project_events(crate::mount::pool_sync::collect(
        stores,
        &Default::default(),
    )?)
}

fn project_events(
    events: crate::mount::pool_sync::Events,
) -> anyhow::Result<Option<(Files, Conflicts)>> {
    if events.is_empty() {
        return Ok(None);
    }
    let projection = crate::mount::peer_projection::project(&events)?;
    let files = projection
        .files
        .into_iter()
        .filter_map(|(path, r)| r.event.content.map(|c| (path, c.size)))
        .collect();
    Ok(Some((files, projection.conflicts)))
}

fn listing(pool: &str, mode: &str, files: Files, conflicts: &Conflicts) -> PoolBrowse {
    let mut all: std::collections::BTreeMap<String, BrowseEntry> = Default::default();
    for (path, size) in files {
        let mut prefix = String::new();
        let parts: Vec<_> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            all.entry(prefix.clone()).or_insert_with(|| BrowseEntry {
                path: prefix.clone(),
                size: 0,
                is_dir: true,
            });
        }
        all.insert(
            path.clone(),
            BrowseEntry {
                path,
                size,
                is_dir: false,
            },
        );
    }
    let mut notes: Vec<String> = conflicts
        .iter()
        .map(|c| {
            let copies: Vec<_> = c
                .branches
                .iter()
                .map(|b| match &b.file {
                    Some(file) => format!("{file} (by {})", b.worker),
                    None => format!("<no visible copy> (by {})", b.worker),
                })
                .collect();
            let mut note = format!("conflict at {}: {}", c.path, copies.join(", "));
            if !c.originals.is_empty() {
                note.push_str(&format!("; original: {}", c.originals.join(", ")));
            }
            if c.original_unavailable {
                note.push_str("; original unavailable");
            }
            if c.ambiguous_original {
                note.push_str("; ambiguous original");
            }
            note
        })
        .collect();
    if mode == "v6" {
        notes.push("v6: explicit empty folders are local to each PC and not listed".into());
    }
    PoolBrowse {
        pool: pool.into(),
        mode: mode.into(),
        entries: all.into_values().collect(),
        notes,
    }
}

#[cfg(test)]
#[path = "browse_tests.rs"]
mod tests;
