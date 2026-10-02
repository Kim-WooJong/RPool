//! Library listing of a pool that is mounted on this PC, read from the
//! running mount's workspace instead of the cloud: no metadata download, and
//! files saved but not uploaded yet are listed too (as the drive shows them).
use super::browse::{BrowseEntry, PoolBrowse};
use crate::mount::namespace::Namespace;
use std::collections::BTreeMap;
use std::path::Path;

/// The listing of `pool` from a running mount's workspace, or None when no
/// mount of it runs here or its namespace cannot be read (the caller then
/// lists the cloud).
pub(crate) fn browse_mounted(pool: &str) -> Option<PoolBrowse> {
    let mount = crate::monitor::active_mounts()
        .into_iter()
        .find(|m| m.pool == pool)?;
    let mut result = listing(Path::new(&mount.workspace))?;
    result.pool = pool.into();
    result.notes.insert(
        0,
        format!(
            "Listed from the drive mounted at {} on this PC (includes files not uploaded yet)",
            mount.mountpoint
        ),
    );
    Some(result)
}

/// Read-only: `Namespace::load` only writes when no checkpoint exists, which
/// the `namespace.json` check rules out.
fn listing(workspace: &Path) -> Option<PoolBrowse> {
    if !workspace.join("namespace.json").is_file() {
        return None;
    }
    let state = Namespace::load(workspace, "library").ok()?;
    let mut files: BTreeMap<String, u64> = state
        .resolved()
        .ok()?
        .into_iter()
        .filter_map(|(path, r)| r.event.content.map(|c| (path, c.size)))
        .collect();
    for intent in &state.pending {
        if intent.spool.is_some() {
            files.insert(intent.path.clone(), intent.size);
        } else {
            files.remove(&intent.path);
        }
    }
    let mut all: BTreeMap<String, BrowseEntry> = BTreeMap::new();
    let add_dirs = |path: &str, all: &mut BTreeMap<String, BrowseEntry>| {
        let mut prefix = String::new();
        for part in path.split('/') {
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
    };
    for dir in &state.directories {
        add_dirs(dir, &mut all);
    }
    for (path, size) in files {
        if let Some((parent, _)) = path.rsplit_once('/') {
            add_dirs(parent, &mut all);
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
    Some(PoolBrowse {
        pool: String::new(),
        mode: "v6-mounted".into(),
        entries: all.into_values().collect(),
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounted_workspace_lists_pending_files_and_folders() {
        let root = tempfile::tempdir().unwrap();
        let drive = crate::mount::virtual_drive::fixture(root.path());
        let core = crate::mount::fs_core::FsCore::new(std::sync::Arc::new(drive)).unwrap();
        core.mkdir("docs").unwrap();
        core.mkdir("empty").unwrap();
        let handle = core
            .open(
                "docs/new.txt",
                crate::mount::fs_core::Access::Write {
                    truncate: true,
                    append: false,
                },
                true,
                false,
            )
            .unwrap();
        core.write_at(handle, 0, b"hello").unwrap();
        core.release(handle).unwrap();
        let listed = listing(root.path()).unwrap();
        let find = |p: &str| listed.entries.iter().find(|e| e.path == p).cloned();
        assert_eq!(
            find("docs/new.txt").map(|e| (e.size, e.is_dir)),
            Some((5, false))
        );
        assert!(find("docs").unwrap().is_dir);
        assert!(find("empty").unwrap().is_dir);
        assert!(listing(&root.path().join("missing")).is_none());
    }
}
