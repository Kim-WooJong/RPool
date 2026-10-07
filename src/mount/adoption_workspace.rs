//! Switches this PC's workspace to an adopted drive generation
//! (`pool migrate adopt --workspace`). The old workspace is renamed to a
//! sibling backup, never deleted; the next mount of the same path starts a
//! fresh workspace that `adoption_fence` initializes on the adopted epoch.
//!
//! Changes that exist only in the old workspace (sealed writes, deletions or
//! commits not yet published when the drive was frozen) were not part of the
//! migration. They are listed, and the written files are exported first with
//! the existing spool recovery (`<backup>/recovered-writes/`, each with its
//! intent receipt naming the drive path), so they can be copied into the
//! new drive.
use super::adoption_fence::workspace_generation;
use super::pool_transition::{assert_no_incomplete_transition, lock_workspace_transition};
use super::virtual_drive::{recover_spool, VirtualDrive};
use crate::migration::drive_model::DriveAdoption;
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of a successful [`switch`], reported by `pool migrate adopt --workspace`.
pub(crate) struct Switched {
    /// Sibling path the old workspace was renamed to (`.<name>.migration-backup-…`).
    pub backup: PathBuf,
    /// Human-readable changes that were only in the old workspace.
    pub local_only: Vec<String>,
    /// Exported spool files (under the backup's `recovered-writes/`).
    pub exported: Vec<PathBuf>,
}

/// `Ok(None)` when there is nothing to switch (no workspace yet, or it is
/// already on the adopted generation).
pub(crate) fn switch(
    rclone: &str,
    workspace: &Path,
    adoption: &DriveAdoption,
) -> Result<Option<Switched>> {
    let _lock = lock_workspace_transition(workspace)?;
    assert_no_incomplete_transition(workspace)?;
    let Some(generation) = workspace_generation(workspace)? else {
        if workspace.join("virtual.json").exists() {
            bail!("{} is not a pool-sync drive workspace", workspace.display());
        }
        return Ok(None);
    };
    if generation == adoption.generation() {
        return Ok(None);
    }
    if generation != adoption.source {
        bail!(
            "{} is on {}, not on the generation this migration adopted from ({}); not switched",
            workspace.display(),
            generation.label(),
            adoption.source.label()
        );
    }
    let local_only = local_only(rclone, workspace)?;
    let exported = if local_only.is_empty() {
        vec![]
    } else {
        recover_spool(workspace)?
    };
    let name = workspace
        .file_name()
        .context("workspace name missing")?
        .to_string_lossy()
        .into_owned();
    let backup = workspace.with_file_name(format!(
        ".{name}.migration-backup-{}",
        &adoption.epoch[..12]
    ));
    if backup.exists() {
        bail!(
            "backup path {} already exists; preserve both and move one aside",
            backup.display()
        );
    }
    crate::utils::rename_dir(workspace, &backup)?;
    #[cfg(unix)]
    File::open(backup.parent().context("workspace parent missing")?)?.sync_all()?;
    let exported = exported
        .into_iter()
        .filter_map(|p| p.strip_prefix(workspace).ok().map(|rel| backup.join(rel)))
        .collect();
    Ok(Some(Switched {
        backup,
        local_only,
        exported,
    }))
}

/// Changes recorded only in this workspace. Refuses a mounted workspace.
fn local_only(rclone: &str, workspace: &Path) -> Result<Vec<String>> {
    let cache = tempfile::tempdir()?;
    let drive = VirtualDrive::open_recovery_source(
        rclone,
        workspace,
        super::shard_cache::ShardCache::new(cache.path().join("cache"), 1 << 20)?,
    )
    .context("stop this drive's mount before switching it")?;
    let state = drive.state.lock().unwrap();
    let mut out = Vec::new();
    for intent in &state.pending {
        out.push(match intent.spool {
            Some(_) => format!("unpublished write: {} ({} bytes)", intent.path, intent.size),
            None => format!("unpublished deletion: {}", intent.path),
        });
    }
    for event in state.committed_intents.values() {
        if !state.published.contains(event) {
            if let Some(e) = state.events.get(event) {
                out.push(format!("committed, not published: {}", e.path));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::drive_model::GenerationRef;

    fn adoption() -> DriveAdoption {
        DriveAdoption {
            version: 1,
            migration_id: "m1".into(),
            source: GenerationRef { epoch: None },
            epoch: "e".repeat(64),
            files: 1,
            dropped: vec![],
            pc_id: "pc".into(),
            ts_unix: 1,
        }
    }

    /// A stopped v6 workspace with one sealed write that was never published.
    fn workspace(root: &Path, epoch: Option<&str>) {
        fs::create_dir_all(root).unwrap();
        let drive = super::super::virtual_drive::fixture(root);
        drive.state.lock().unwrap().version = 6;
        drive.state.lock().unwrap().save(root).unwrap();
        let intent = drive.begin_intent("notes.txt", None).unwrap();
        fs::write(drive.spool_path(&intent), b"only on this PC").unwrap();
        drive.seal(intent).unwrap();
        drop(drive);
        let mut binding = serde_json::json!({
            "version": 6,
            "pool": "p",
            "shared": "a:x/.rpool-sync/events-v6/s",
            "policy": PoolDefinition::default(),
            "metadata_roots": ["a:x/.rpool-sync/events-v6/s"],
        });
        if let Some(epoch) = epoch {
            binding["epoch"] = epoch.into();
        }
        fs::write(root.join("virtual.json"), binding.to_string()).unwrap();
    }

    #[test]
    fn old_workspace_becomes_a_backup_and_local_writes_are_exported() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("drive");
        workspace(&ws, None);
        let switched = switch("no-rclone", &ws, &adoption()).unwrap().unwrap();
        assert!(!ws.exists(), "the next mount starts a fresh workspace");
        assert!(switched.backup.join("virtual.json").exists());
        assert!(switched.backup.join("namespace.json").exists());
        assert!(
            switched
                .local_only
                .iter()
                .any(|c| c.contains("unpublished write: notes.txt")),
            "{:?}",
            switched.local_only
        );
        assert_eq!(switched.exported.len(), 1);
        assert_eq!(fs::read(&switched.exported[0]).unwrap(), b"only on this PC");
        // Nothing left to switch: the next mount opens the adopted epoch.
        assert!(switch("no-rclone", &ws, &adoption()).unwrap().is_none());
    }

    #[test]
    fn workspace_on_another_generation_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("drive");
        workspace(&ws, Some(&"b".repeat(64)));
        let error = switch("no-rclone", &ws, &adoption()).unwrap_err();
        assert!(error.to_string().contains("not switched"), "{error}");
        assert!(ws.join("virtual.json").exists());
        // Already on the adopted generation: nothing to do.
        let on_new = dir.path().join("new");
        workspace(&on_new, Some(&"e".repeat(64)));
        assert!(switch("no-rclone", &on_new, &adoption()).unwrap().is_none());
        assert!(on_new.exists());
    }
}
