//! Explicit, resumable membership changes. Never sync or delete the original workspace.
//!
//! `rpool mount --apply-pool-changes` (`run`): copies the visible files of a
//! pool-sync workspace into a staged workspace under a new epoch (with the
//! pool's current membership), then swaps it in and keeps the original as a
//! backup. Progress lives in a durable journal next to the workspace
//! (`.<name>.pool-transition.json`), so an interrupted run resumes.
use super::account_recovery as recovery;
use super::namespace::{durable_json, random_id, Intent};
use super::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;

/// Progress of one visible path in the transition journal.
#[derive(Serialize, Deserialize)]
struct Entry {
    /// Source revision id being copied (must not change across resumes).
    revision: String,
    /// Destination intent created for the copy; `None` until begun.
    intent: Option<Intent>,
    /// Copy uploaded and verified in the destination.
    complete: bool,
}
/// Durable transition state, persisted after every step by `persist`.
#[derive(Serialize, Deserialize)]
struct Journal {
    /// Journal format version (1).
    version: u32,
    /// Canonical workspace path being transitioned.
    workspace: PathBuf,
    /// Staging workspace (`.<name>.pool-stage-<epoch>`) built by the copy.
    stage: PathBuf,
    /// Backup path (`.<name>.pool-backup-<epoch>`) the original moves to.
    backup: PathBuf,
    /// New metadata epoch of the destination (64 hex).
    epoch: String,
    /// Selected pool name.
    pool: String,
    /// Hash of the pool definition plus its remote roots; a change refuses resume.
    policy_hash: String,
    /// Source workspace file hashes (`account_recovery::source_hashes`); a change
    /// refuses resume and activation.
    source_hashes: BTreeMap<String, String>,
    /// Canonical reprocess plan whose completed replacements are used, if any.
    reprocess_plan: Option<PathBuf>,
    /// Per visible path progress.
    entries: BTreeMap<String, Entry>,
    /// Every entry is complete and synced; activation may start.
    ready: bool,
    /// Stage and original have been swapped (the transition is done).
    activated: bool,
}

/// Crash-safely writes the journal to `path`.
fn persist(path: &Path, journal: &Journal) -> Result<()> {
    durable_json(path, journal)
}

// Refuse unrelated edits even when they do not collide with a copied path.
/// Fails if the destination workspace contains pending intents, events or
/// directories that the transition itself did not create.
fn check_destination(
    drive: &VirtualDrive,
    journal: &Journal,
    directories: &BTreeSet<String>,
) -> Result<()> {
    let ids: BTreeSet<String> = journal
        .entries
        .values()
        .filter_map(|e| e.intent.as_ref().map(|i| i.id.clone()))
        .collect();
    let state = drive.state.lock().unwrap();
    let events: BTreeSet<String> = state
        .committed_intents
        .iter()
        .filter(|(id, _)| ids.contains(*id))
        .map(|(_, event)| event.clone())
        .collect();
    if state.pending.iter().any(|i| !ids.contains(&i.id))
        || state.events.keys().any(|id| !events.contains(id))
        || !state.directories.is_subset(directories)
    {
        bail!("transition destination changed; preserve stage and original workspace");
    }
    Ok(())
}

/// Flushes the parent directory of `path` after a rename (no-op off Unix).
fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path.parent().context("workspace parent missing")?)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The journal is written before either rename. Filesystem state disambiguates a
/// crash after a rename but before its journal update; nothing is removed.
fn activate(journal_path: &Path, journal: &mut Journal) -> Result<()> {
    if !journal.ready {
        bail!("transition is not ready for activation");
    }
    if journal.stage.exists() {
        if !journal.backup.exists() {
            if !journal.workspace.exists() {
                bail!("transition source missing");
            }
            fs::rename(&journal.workspace, &journal.backup)?;
            sync_parent(&journal.workspace)?;
        } else if journal.workspace.exists() {
            bail!("transition activation paths are ambiguous; all workspaces retained");
        }
        fs::rename(&journal.stage, &journal.workspace)?;
        sync_parent(&journal.workspace)?;
    } else if !journal.workspace.exists() || !journal.backup.exists() {
        bail!("transition activation paths missing; all remaining data retained");
    }
    journal.activated = true;
    persist(journal_path, journal)
}

/// Runs (or resumes) a pool membership transition for `args.workspace`;
/// called by `mount::run` for `--apply-pool-changes`.
pub(crate) fn run(rclone: &str, args: &crate::cli::MountArgs) -> Result<()> {
    let _transition_lock = lock_workspace_transition(&args.workspace)?;
    let _stop = super::lifecycle::StopControl::new(args.stop_file.clone())?;
    recovery::check_stop(args.stop_file.as_deref())?;
    if args.account_recovery_from.is_some() {
        bail!("apply pool changes requires the selected virtual pool-sync workspace, not account recovery");
    }
    let parent = args
        .workspace
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    recovery::no_symlink_ancestors(parent)?;
    let workspace = parent.canonicalize()?.join(
        args.workspace
            .file_name()
            .context("workspace name missing")?,
    );
    let journal_path = workspace.with_file_name(format!(
        ".{}.pool-transition.json",
        workspace.file_name().unwrap().to_string_lossy()
    ));
    let policy = crate::pool::load_pool_store()?
        .pools
        .get(&args.pool)
        .cloned()
        .context("unknown selected pool")?;
    let allowed = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
    let policy_hash = blake3::hash(&serde_json::to_vec(&(&policy, &allowed))?)
        .to_hex()
        .to_string();
    let plan = args
        .recovery_reprocess_plan
        .as_ref()
        .map(|p| p.canonicalize())
        .transpose()?;
    let mut saved: Option<Journal> = if journal_path.exists() {
        recovery::no_symlinks(&journal_path)?;
        Some(crate::utils::read_json(&journal_path)?)
    } else {
        None
    };
    if let Some(j) = &saved {
        let name = workspace.file_name().unwrap().to_string_lossy();
        if j.version != 1
            || j.workspace != workspace
            || j.pool != args.pool
            || j.policy_hash != policy_hash
            || j.reprocess_plan != plan
            || j.epoch.len() != 64
            || !j.epoch.bytes().all(|c| c.is_ascii_hexdigit())
            || j.stage != workspace.with_file_name(format!(".{name}.pool-stage-{}", j.epoch))
            || j.backup != workspace.with_file_name(format!(".{name}.pool-backup-{}", j.epoch))
        {
            bail!(
                "transition identity or destination configuration changed; preserve all workspaces"
            );
        }
    }
    let source_path = saved
        .as_ref()
        .filter(|j| j.backup.exists())
        .map(|j| j.backup.clone())
        .unwrap_or_else(|| workspace.clone());
    recovery::no_symlinks(&source_path)?;
    super::adapter::preflight_virtual(&source_path)?;
    if !journal_path.exists() {
        // A pool migration owns this drive's next generation.
        super::adoption_fence::check_transition_source(rclone, &args.pool, &source_path)?;
    }
    let temporary_cache = tempfile::tempdir()?;
    let cache_limit = args
        .cache_gib
        .checked_mul(1073741824)
        .context("cache limit overflow")?;
    let source = VirtualDrive::open_recovery_source(
        rclone,
        &source_path,
        super::shard_cache::ShardCache::new(temporary_cache.path().join("cache"), cache_limit)?,
    )?;
    if source.pool != args.pool {
        bail!("transition must preserve selected pool name");
    }
    let hashes = recovery::source_hashes(&source)?;
    let aliases = |remotes: &[String]| -> BTreeSet<String> {
        remotes
            .iter()
            .map(|r| r.split(':').next().unwrap_or(r).to_string())
            .collect()
    };
    let excluded: BTreeSet<String> = aliases(&source.policy.remotes)
        .difference(&aliases(&policy.remotes))
        .cloned()
        .collect();
    let _ = crate::storage::reader::StorageReader::rclone_with_excluded_remotes(rclone, &excluded)?;
    let replacements = plan
        .as_ref()
        .map(|p| crate::pool::completed_reprocess_replacements(p))
        .transpose()?
        .unwrap_or_default();
    let mut journal = if let Some(j) = saved.take() {
        if j.source_hashes != hashes {
            bail!("original source changed; transition cannot resume");
        }
        j
    } else {
        let epoch = random_id()?;
        let name = workspace.file_name().unwrap().to_string_lossy();
        Journal {
            version: 1,
            stage: workspace.with_file_name(format!(".{name}.pool-stage-{epoch}")),
            backup: workspace.with_file_name(format!(".{name}.pool-backup-{epoch}")),
            workspace: workspace.clone(),
            epoch,
            pool: args.pool.clone(),
            policy_hash,
            source_hashes: hashes,
            reprocess_plan: plan,
            entries: BTreeMap::new(),
            ready: false,
            activated: false,
        }
    };
    persist(&journal_path, &journal)?;
    if journal.activated {
        recovery::no_symlinks(&workspace)?;
        super::adapter::preflight_virtual(&workspace)?;
        let binding: serde_json::Value = crate::utils::read_json(&workspace.join("virtual.json"))?;
        if binding.get("epoch").and_then(|v| v.as_str()) != Some(&journal.epoch) {
            bail!("activated workspace identity changed");
        }
        fs::rename(
            &journal_path,
            journal
                .backup
                .join(format!("pool-transition-{}-completed.json", journal.epoch)),
        )?;
        sync_parent(&journal_path)?;
        println!(
            "Pool changes already applied. Original retained at {}",
            journal.backup.display()
        );
        return Ok(());
    }
    let destination_path = if journal.ready && !journal.stage.exists() {
        workspace.clone()
    } else {
        journal.stage.clone()
    };
    if destination_path.exists() {
        recovery::no_symlinks(&destination_path)?;
    }
    if !destination_path.join("virtual.json").exists() && destination_path.exists() {
        if journal.ready || destination_path != journal.stage {
            bail!("transition destination binding missing");
        }
        // A crash between lock creation and binding publication can leave a bootstrap
        // fragment. Preserve it under a unique sibling; never delete or adopt it.
        let retained = destination_path
            .with_file_name(format!(".rpool-transition-bootstrap-{}", random_id()?));
        fs::rename(&destination_path, &retained)?;
        sync_parent(&destination_path)?;
    }
    if !destination_path.join("virtual.json").exists() {
        if destination_path.exists() {
            bail!("unpublished transition stage exists; preserve it");
        }
        let initializing = tempfile::Builder::new()
            .prefix(".rpool-transition-initialize-")
            .tempdir_in(parent.canonicalize()?)?
            .keep();
        let initialized = VirtualDrive::open_with_epoch(
            rclone,
            &args.pool,
            &initializing,
            "pool-transition",
            cache_limit,
            &journal.epoch,
        )?;
        drop(initialized);
        fs::rename(&initializing, &destination_path)?;
        sync_parent(&destination_path)?;
    }
    let mut destination = VirtualDrive::open(
        rclone,
        &args.pool,
        &destination_path,
        "pool-transition",
        cache_limit,
    )?;
    let binding: serde_json::Value =
        crate::utils::read_json(&destination_path.join("virtual.json"))?;
    if binding.get("epoch").and_then(|v| v.as_str()) != Some(&journal.epoch) {
        bail!("transition staging epoch mismatch; preserve workspaces");
    }
    destination.spool_limit = args
        .spool_gib
        .checked_mul(1073741824)
        .context("spool limit overflow")?;
    let directories = source.state.lock().unwrap().directories.clone();
    let view = source.view()?;
    if journal.entries.keys().any(|p| !view.contains_key(p)) {
        bail!("transition source view changed");
    }
    check_destination(&destination, &journal, &directories)?;
    destination.pull()?;
    check_destination(&destination, &journal, &directories)?;
    println!("Applying pool changes: copying {} locally known visible files, conflicts and sealed pending writes. Historical versions and native dirty cache remain only in the original backup; remote-only changes are not migrated.", view.len());
    for (path, revision) in &view {
        recovery::check_stop(args.stop_file.as_deref())?;
        journal
            .entries
            .entry(path.clone())
            .or_insert_with(|| Entry {
                revision: revision.id().into(),
                intent: None,
                complete: false,
            });
        if journal.entries[path].revision != revision.id() {
            bail!("source revision changed");
        }
        if journal.entries[path].complete {
            recovery::completion_valid(
                &destination,
                path,
                journal.entries[path]
                    .intent
                    .as_ref()
                    .context("completion intent missing")?,
            )?;
            continue;
        }
        if journal.entries[path].intent.is_none() {
            if destination.view()?.contains_key(path) {
                bail!("transition destination path changed");
            }
            journal.entries.get_mut(path).unwrap().intent =
                Some(destination.begin_intent(path, None)?);
            persist(&journal_path, &journal)?;
        }
        let intent = journal.entries[path].intent.as_ref().unwrap().clone();
        let state = destination.state.lock().unwrap();
        let committed = state.committed_intents.contains_key(&intent.id);
        let pending = state.pending.iter().any(|i| i.id == intent.id);
        drop(state);
        if !committed && !pending {
            if destination.view()?.contains_key(path) {
                bail!("transition destination path changed");
            }
            if let Some(manifest) =
                recovery::matching_replacement(revision, &replacements, &allowed)?
            {
                let hash = recovery::verify_replacement(
                    &source,
                    revision,
                    &manifest,
                    &excluded,
                    args.stop_file.as_deref(),
                )?;
                let replacement = Revision::Cloud {
                    id: revision.id().into(),
                    content: super::shared_model::Content {
                        hash: hash.clone(),
                        size: manifest.original_size,
                        manifest: manifest.clone(),
                        pack: None,
                    },
                };
                // Read the verified replacement directly into a fresh,
                // destination-owned upload.
                let mut target = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(destination.spool_path(&intent))?;
                let mut offset = 0;
                while offset < replacement.size() {
                    recovery::check_stop(args.stop_file.as_deref())?;
                    let reader =
                        crate::storage::reader::StorageReader::rclone_with_excluded_remotes(
                            rclone, &excluded,
                        )?;
                    let bytes = source.cache.read(
                        &reader,
                        &manifest,
                        offset,
                        (replacement.size() - offset).min(1024 * 1024) as usize,
                        source.policy.workers,
                        source.policy.retries,
                    )?;
                    if bytes.is_empty() {
                        bail!("replacement short read");
                    }
                    destination.write_spool_bytes(&mut target, &bytes)?;
                    offset += bytes.len() as u64;
                }
                target.sync_all()?;
                if crate::utils::hash_file_range(
                    &destination.spool_path(&intent),
                    0,
                    replacement.size(),
                )? != hash
                {
                    bail!("replacement copy hash mismatch");
                }
            } else {
                recovery::copy_revision(
                    &source,
                    &destination,
                    revision,
                    &destination.spool_path(&intent),
                    &excluded,
                    args.stop_file.as_deref(),
                )?;
            }
            destination.seal(intent.clone())?;
        }
        check_destination(&destination, &journal, &directories)?;
        destination.sync()?;
        recovery::completion_valid(&destination, path, &intent)?;
        journal.entries.get_mut(path).unwrap().complete = true;
        persist(&journal_path, &journal)?;
    }
    {
        let mut state = destination.state.lock().unwrap();
        state.directories.extend(directories.iter().cloned());
        state.save(&destination_path)?;
    }
    destination.sync()?;
    check_destination(&destination, &journal, &directories)?;
    for (path, entry) in &journal.entries {
        recovery::completion_valid(
            &destination,
            path,
            entry.intent.as_ref().context("completion intent missing")?,
        )?;
    }
    if recovery::source_hashes(&source)? != journal.source_hashes {
        bail!("source changed before activation");
    }
    recovery::check_stop(args.stop_file.as_deref())?;
    journal.ready = true;
    persist(&journal_path, &journal)?;
    drop(destination);
    drop(source);
    activate(&journal_path, &mut journal)?;
    fs::rename(
        &journal_path,
        journal
            .backup
            .join(format!("pool-transition-{}-completed.json", journal.epoch)),
    )?;
    sync_parent(&journal_path)?;
    println!("Pool changes applied at {}. Original workspace and history retained at {}. No original cloud objects deleted.", workspace.display(), journal.backup.display());
    Ok(())
}

/// Called before ordinary opening, which must never recreate a half-renamed source.
pub(crate) fn assert_no_incomplete_transition(workspace: &Path) -> Result<()> {
    let parent = workspace
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.exists() {
        return Ok(());
    }
    let journal_path = parent.canonicalize()?.join(format!(
        ".{}.pool-transition.json",
        workspace
            .file_name()
            .context("workspace name missing")?
            .to_string_lossy()
    ));
    if journal_path.exists() {
        recovery::no_symlinks(&journal_path)?;
        let j: Journal = crate::utils::read_json(&journal_path)?;
        if !j.activated {
            bail!("pool membership transition incomplete; resume with --apply-pool-changes before mounting");
        }
    }
    Ok(())
}

/// Serialized with ordinary mount startup and held for its entire lifetime.
pub(crate) struct TransitionLock(File);
impl Drop for TransitionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
/// Takes the exclusive `.<name>.pool-transition.lock` file lock next to the
/// workspace. Held by `mount::run` for the whole mount and by `run`.
pub(crate) fn lock_workspace_transition(workspace: &Path) -> Result<TransitionLock> {
    let parent = workspace
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    recovery::no_symlink_ancestors(parent)?;
    let path = parent.canonicalize()?.join(format!(
        ".{}.pool-transition.lock",
        workspace
            .file_name()
            .context("workspace name missing")?
            .to_string_lossy()
    ));
    if path.exists() {
        recovery::no_symlinks(&path)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    lock.try_lock()
        .context("workspace mount or pool transition is already active")?;
    Ok(TransitionLock(lock))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal(root: &Path) -> Journal {
        Journal {
            version: 1,
            workspace: root.join("selected"),
            stage: root.join("stage"),
            backup: root.join("backup"),
            epoch: "a".repeat(64),
            pool: "pool".into(),
            policy_hash: "hash".into(),
            source_hashes: BTreeMap::new(),
            reprocess_plan: None,
            entries: BTreeMap::new(),
            ready: true,
            activated: false,
        }
    }
    #[test]
    fn activation_resumes_each_rename_without_deleting_source() {
        for interrupted in 0..=2 {
            let dir = tempfile::tempdir().unwrap();
            let mut j = journal(dir.path());
            fs::create_dir(&j.workspace).unwrap();
            fs::write(j.workspace.join("original"), b"history and dirty cache").unwrap();
            fs::create_dir(&j.stage).unwrap();
            fs::write(j.stage.join("new"), b"verified").unwrap();
            let marker = dir.path().join("journal.json");
            persist(&marker, &j).unwrap();
            if interrupted >= 1 {
                fs::rename(&j.workspace, &j.backup).unwrap();
            }
            if interrupted == 2 {
                fs::rename(&j.stage, &j.workspace).unwrap();
            }
            activate(&marker, &mut j).unwrap();
            assert!(j.activated);
            assert_eq!(
                fs::read(j.backup.join("original")).unwrap(),
                b"history and dirty cache"
            );
            assert_eq!(fs::read(j.workspace.join("new")).unwrap(), b"verified");
            assert!(!j.stage.exists());
        }
    }
    #[test]
    fn activation_rejects_ambiguous_and_unverified_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = journal(dir.path());
        for p in [&j.workspace, &j.stage, &j.backup] {
            fs::create_dir(p).unwrap();
        }
        assert!(activate(&dir.path().join("journal.json"), &mut j).is_err());
        assert!(j.workspace.exists() && j.stage.exists() && j.backup.exists());
        j.ready = false;
        assert!(activate(&dir.path().join("journal.json"), &mut j).is_err());
    }
    #[test]
    fn destination_rejects_unrelated_pending_writes_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let drive = super::super::virtual_drive::fixture(dir.path());
        let j = journal(dir.path());
        let dirs = BTreeSet::new();
        check_destination(&drive, &j, &dirs).unwrap();
        drive
            .state
            .lock()
            .unwrap()
            .directories
            .insert("unrelated".into());
        assert!(check_destination(&drive, &j, &dirs).is_err());
        drive.state.lock().unwrap().directories.clear();
        let intent = drive.begin_intent("unrelated.txt", None).unwrap();
        fs::write(drive.spool_path(&intent), b"unrelated write").unwrap();
        drive.seal(intent).unwrap();
        assert!(check_destination(&drive, &j, &dirs).is_err());
    }
    #[test]
    fn ordinary_mount_is_blocked_even_when_selected_path_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let j = journal(dir.path());
        persist(&dir.path().join(".selected.pool-transition.json"), &j).unwrap();
        assert!(assert_no_incomplete_transition(&j.workspace).is_err());
    }
    #[test]
    fn transition_and_mount_share_exclusive_lock_with_explicit_release() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("selected");
        let lock = lock_workspace_transition(&workspace).unwrap();
        let inherited = lock.0.try_clone().unwrap();
        assert!(lock_workspace_transition(&workspace).is_err());
        drop(lock);
        let next = lock_workspace_transition(&workspace).unwrap();
        assert!(lock_workspace_transition(&workspace).is_err());
        drop(next);
        drop(inherited);
    }
}
