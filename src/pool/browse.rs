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
    /// "v6", "v7" or "none" (the pool has no pool-sync drive yet).
    pub mode: String,
    /// Sorted by path; directories are listed explicitly (also implied ones).
    pub entries: Vec<BrowseEntry>,
    /// Conflict copies and other notes worth showing, human readable.
    pub notes: Vec<String>,
}

/// Lists the drive of `pool` from its cloud metadata. Read-only. Blocking:
/// call it off the UI thread.
///
/// v7 (private snapshots) is preferred when present: a pool moved to v7
/// keeps its legacy v6 events untouched, and a v7 mount shows the v7 drive.
/// v7 is interpreted by a throwaway pool-sync workspace in a temp dir whose
/// pull only lists/reads records (no publication, registration or upload);
/// v6 events are collected and projected directly without any workspace.
pub(crate) fn browse(rclone: &str, pool: &str) -> anyhow::Result<PoolBrowse> {
    use anyhow::Context;
    let policy = super::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .with_context(|| format!("pool not found: {pool}"))?;
    super::validate_pool(&policy)?;
    if let Some((files, conflicts)) = crate::mount::pool_sync::browse_v7(rclone, pool)? {
        return Ok(listing(pool, "v7", files, &conflicts));
    }
    let transports = crate::mount::pool_sync::read_stores(rclone, pool, &policy)?;
    let stores: Vec<_> = transports.iter().map(|t| t.as_ref()).collect();
    match project_v6(&stores)? {
        Some((files, conflicts)) => Ok(listing(pool, "v6", files, &conflicts)),
        None => Ok(PoolBrowse {
            pool: pool.into(),
            mode: "none".into(),
            ..Default::default()
        }),
    }
}

type Files = std::collections::BTreeMap<String, u64>;
type Conflicts = Vec<crate::mount::peer_projection::Conflict>;

/// `None` when no v6 events exist in any replica.
fn project_v6(
    stores: &[&dyn crate::mount::pool_sync::EventStore],
) -> anyhow::Result<Option<(Files, Conflicts)>> {
    let events = crate::mount::pool_sync::collect(stores, &Default::default())?;
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
