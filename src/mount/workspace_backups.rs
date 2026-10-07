//! Backups of a drive workspace that switches leave next to it, and their
//! removal (`rpool drive backups`, GUI Drive › Workspace backups).
//!
//! - `.<name>.migration-backup-<epoch>`: the workspace before a mount (or
//!   `pool migrate adopt --workspace`) switched it to an adopted generation
//!   (`adoption_workspace`).
//! - `.<name>.pool-backup-<epoch>`: the workspace before Apply pool changes
//!   (`pool_transition`).
//!
//! They hold the old namespace, spool and caches; the drive's data is in the
//! cloud. What may only exist there are the exported local-only writes
//! (`recovered-writes/`): removing a backup with them needs an explicit
//! `include_recovered`. A backup still named by an unfinished transition
//! journal, or opened by a mount, is never removed.
use crate::prelude::*;

/// Which switch left a backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BackupKind {
    /// Switch to an adopted drive generation (pool migration).
    Migration,
    /// Apply pool changes (pool transition).
    Transition,
}

/// One backup folder of a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkspaceBackup {
    /// Full path of the backup folder.
    pub path: PathBuf,
    /// Which switch left it.
    pub kind: BackupKind,
    /// Bytes of every file in it (symlinks are not followed).
    pub bytes: u64,
    /// Last modification of the folder (Unix seconds), about when it was made.
    pub modified_unix: u64,
    /// Exported local-only writes in `recovered-writes/` (data files only).
    pub recovered_files: usize,
}

/// Backups next to `workspace`, oldest first; none when its folder is missing.
pub(crate) fn list(workspace: &Path) -> Result<Vec<WorkspaceBackup>> {
    let name = workspace
        .file_name()
        .context("workspace name missing")?
        .to_string_lossy()
        .into_owned();
    let parent = workspace
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Ok(entries) = fs::read_dir(parent) else {
        return Ok(vec![]);
    };
    let kinds = [
        (format!(".{name}.migration-backup-"), BackupKind::Migration),
        (format!(".{name}.pool-backup-"), BackupKind::Transition),
    ];
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(kind) = kinds
            .iter()
            .find(|(prefix, _)| file_name.starts_with(prefix.as_str()))
            .map(|(_, kind)| *kind)
        else {
            continue;
        };
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir() {
            continue;
        }
        let path = entry.path();
        let recovered = path.join("recovered-writes");
        found.push(WorkspaceBackup {
            bytes: tree_bytes(&path),
            modified_unix: metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
            recovered_files: count_data_files(&recovered),
            path,
            kind,
        });
    }
    found.sort_by(|a, b| (a.modified_unix, &a.path).cmp(&(b.modified_unix, &b.path)));
    Ok(found)
}

/// Deletes the backup at `backup` of `workspace` (it must be one [`list`]
/// reports). Refuses one with exported local-only writes unless
/// `include_recovered`, one an unfinished transition still names, and one a
/// process holds open (its `virtual.lock`). Returns the bytes freed.
pub(crate) fn remove(workspace: &Path, backup: &Path, include_recovered: bool) -> Result<u64> {
    let known = list(workspace)?;
    let wanted = backup
        .canonicalize()
        .unwrap_or_else(|_| backup.to_path_buf());
    let Some(entry) = known
        .into_iter()
        .find(|b| b.path.canonicalize().unwrap_or_else(|_| b.path.clone()) == wanted)
    else {
        bail!(
            "{} is not a backup of {}",
            backup.display(),
            workspace.display()
        );
    };
    if entry.recovered_files > 0 && !include_recovered {
        bail!(
            "{} holds {} exported local-only file(s) in recovered-writes/ that may exist nowhere else; copy what you need into the drive, then remove it with include recovered writes",
            entry.path.display(),
            entry.recovered_files
        );
    }
    let name = workspace
        .file_name()
        .context("workspace name missing")?
        .to_string_lossy()
        .into_owned();
    let journal = workspace.with_file_name(format!(".{name}.pool-transition.json"));
    if journal.exists() {
        bail!(
            "an unfinished pool transition of {} still uses its backups; finish it first",
            workspace.display()
        );
    }
    let lock = entry.path.join("virtual.lock");
    if lock.exists() {
        let file = OpenOptions::new().read(true).write(true).open(&lock)?;
        file.try_lock().with_context(|| {
            format!(
                "{} is in use (mounted or opened); stop that first",
                entry.path.display()
            )
        })?;
    }
    fs::remove_dir_all(&entry.path)
        .with_context(|| format!("removing {}", entry.path.display()))?;
    Ok(entry.bytes)
}

/// Sum of file sizes below `root`, not following symlinks; unreadable parts
/// count as 0.
fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

/// Exported writes in `dir`: files that are not `.json` receipts.
fn count_data_files(dir: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if entry.path().extension().is_none_or(|e| e != "json") {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(parent: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let path = parent.join(name);
        for (rel, bytes) in files {
            let file = path.join(rel);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
        path
    }

    #[test]
    fn lists_and_removes_only_this_workspaces_backups() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("drive");
        fs::create_dir_all(&ws).unwrap();
        let plain = backup(
            dir.path(),
            ".drive.migration-backup-aaaaaaaaaaaa",
            &[("namespace.json", b"12345")],
        );
        let with_writes = backup(
            dir.path(),
            ".drive.pool-backup-bbbb",
            &[
                ("recovered-writes/x.sealed.bin", b"data"),
                ("recovered-writes/x.sealed.bin.json", b"{}"),
            ],
        );
        backup(dir.path(), ".other.migration-backup-cccc", &[("a", b"1")]);
        backup(dir.path(), ".drive.pool-stage-dddd", &[("a", b"1")]);

        let found = list(&ws).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        let get = |p: &Path| found.iter().find(|b| b.path == p).unwrap().clone();
        assert_eq!(
            (
                get(&plain).kind,
                get(&plain).bytes,
                get(&plain).recovered_files
            ),
            (BackupKind::Migration, 5, 0)
        );
        assert_eq!(
            (get(&with_writes).kind, get(&with_writes).recovered_files),
            (BackupKind::Transition, 1)
        );

        assert_eq!(remove(&ws, &plain, false).unwrap(), 5);
        assert!(!plain.exists());
        // Exported writes need the explicit flag.
        assert!(remove(&ws, &with_writes, false).is_err());
        assert!(with_writes.exists());
        remove(&ws, &with_writes, true).unwrap();
        assert!(!with_writes.exists());
        // Anything else is refused, even next to the workspace.
        assert!(remove(&ws, &dir.path().join(".other.migration-backup-cccc"), true).is_err());
        assert!(remove(&ws, &dir.path().join(".drive.pool-stage-dddd"), true).is_err());
        assert!(remove(&ws, &ws, true).is_err());
    }

    #[test]
    fn an_unfinished_transition_or_an_open_backup_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("drive");
        let old = backup(dir.path(), ".drive.pool-backup-e", &[("virtual.lock", b"")]);
        fs::write(dir.path().join(".drive.pool-transition.json"), b"{}").unwrap();
        assert!(remove(&ws, &old, true).is_err());
        fs::remove_file(dir.path().join(".drive.pool-transition.json")).unwrap();
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(old.join("virtual.lock"))
            .unwrap();
        held.try_lock().unwrap();
        assert!(remove(&ws, &old, true).is_err());
        drop(held);
        remove(&ws, &old, true).unwrap();
    }
}
