//! Explicit copy-based recovery into a different v6 pool. Source is never synchronized.
use super::namespace::{durable_json, Intent};
use super::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;

#[derive(Serialize, Deserialize)]
struct Entry {
    source_revision: String,
    intent: Option<Intent>,
    complete: bool,
    #[serde(default)]
    failed: bool,
    #[serde(default)]
    reused: bool,
    #[serde(default)]
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    source: PathBuf,
    source_hashes: BTreeMap<String, String>,
    destination: PathBuf,
    approved: bool,
    directories_copied: bool,
    destination_policy_hash: String,
    reprocess_plan: Option<PathBuf>,
    destination_pool: String,
    excluded: BTreeSet<String>,
    entries: BTreeMap<String, Entry>,
}

pub(super) fn no_symlink_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            bail!("recovery does not accept symlink paths");
        }
    }
    Ok(())
}
pub(super) fn no_symlinks(path: &Path) -> Result<()> {
    no_symlink_ancestors(path)?;
    fn visit(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            bail!("recovery workspace contains a symlink");
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(&entry?.path())?;
            }
        } else if !metadata.is_file() {
            bail!("recovery workspace contains a special file");
        }
        Ok(())
    }
    visit(path)
}
fn digest(path: &Path) -> Result<String> {
    Ok(blake3::hash(&fs::read(path)?).to_hex().to_string())
}
pub(super) fn check_stop(stop: Option<&Path>) -> Result<()> {
    if stop.is_some_and(Path::exists) {
        bail!("recovery stopped; source and destination retained for resume");
    }
    Ok(())
}
pub(super) fn copy_revision(
    source: &VirtualDrive,
    destination: &VirtualDrive,
    revision: &Revision,
    output: &Path,
    excluded: &BTreeSet<String>,
    stop: Option<&Path>,
) -> Result<()> {
    let mut target = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output)?;
    match revision {
        Revision::Local { path, size, id, .. } => {
            let state = source.state.lock().unwrap();
            let intent = state
                .pending
                .iter()
                .find(|i| &i.id == id)
                .context("source intent missing")?;
            if fs::metadata(path)?.len() != *size
                || crate::utils::hash_file_range(path, 0, *size)? != intent.hash
            {
                bail!("source retained write integrity failure");
            }
            let mut input = File::open(path)?;
            let mut buffer = vec![0u8; 1024 * 1024];
            loop {
                check_stop(stop)?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                destination.write_spool_bytes(&mut target, &buffer[..count])?;
            }
        }
        Revision::Cloud { content, .. } => {
            let manifest = content.manifest.clone();
            crate::manifest::validate_manifest(&manifest)?;
            if manifest.original_size != revision.size() {
                bail!("source revision size mismatch");
            }
            let mut offset = 0;
            while offset < revision.size() {
                let count = (revision.size() - offset).min(1024 * 1024) as usize;
                check_stop(stop)?;
                let reader = crate::storage::reader::StorageReader::rclone_with_excluded_remotes(
                    &source.rclone,
                    excluded,
                )?;
                let bytes = source.cache.read(
                    &reader,
                    &manifest,
                    offset,
                    count,
                    source.policy.workers,
                    source.policy.retries,
                )?;
                if bytes.is_empty() || bytes.len() > count {
                    bail!("invalid recovery read length");
                }
                destination.write_spool_bytes(&mut target, &bytes)?;
                offset += bytes.len() as u64;
            }
        }
    }
    target.sync_all()?;
    let expected = match revision {
        Revision::Local { id, .. } => Some(
            source
                .state
                .lock()
                .unwrap()
                .pending
                .iter()
                .find(|i| &i.id == id)
                .context("source intent missing")?
                .hash
                .clone(),
        ),
        Revision::Cloud { content, .. } if content.hash != content.manifest.content_root_blake3 => {
            Some(content.hash.clone())
        }
        _ => None,
    };
    if target.metadata()?.len() != revision.size() {
        bail!("recovery length mismatch");
    }
    let actual = crate::utils::hash_file_range(output, 0, revision.size())?;
    if expected.is_some_and(|hash| hash != actual) {
        bail!("recovery full-file hash mismatch");
    }
    Ok(())
}

pub(super) fn source_hashes(source: &VirtualDrive) -> Result<BTreeMap<String, String>> {
    // Namespace saves normally append to the journal and leave the checkpoint
    // unchanged; it is listed only when present so earlier records still match.
    let journal = "namespace.journal";
    let journaled = source.root.join(journal).exists().then_some(journal);
    ["virtual.json", "namespace.json"]
        .into_iter()
        .chain(journaled)
        .map(|name| Ok((name.into(), digest(&source.root.join(name))?)))
        .collect()
}
pub(super) fn completion_valid(drive: &VirtualDrive, path: &str, intent: &Intent) -> Result<()> {
    let state = drive.state.lock().unwrap();
    let event = state
        .committed_intents
        .get(&intent.id)
        .context("recovered file not committed")?;
    if !state.published.contains(event)
        || state
            .resolved()?
            .get(path)
            .is_none_or(|r| &r.event_id != event)
        || state.pending.iter().any(|i| i.path == path)
    {
        bail!("destination changed or metadata is not fully published");
    }
    Ok(())
}
fn approve_destination(drive: &VirtualDrive, approved: bool) -> Result<()> {
    if !approved {
        let state = drive.state.lock().unwrap();
        if !state.events.is_empty() || !state.pending.is_empty() || !state.directories.is_empty() {
            bail!("destination pool already has metadata; choose a new pool identity");
        }
    }
    Ok(())
}
pub(super) fn matching_replacement(
    revision: &Revision,
    replacements: &[(Manifest, Manifest)],
    allowed: &[String],
) -> Result<Option<Manifest>> {
    let Revision::Cloud { content, .. } = revision else {
        return Ok(None);
    };
    let retained = content.manifest.clone();
    let fingerprint = crate::manifest::manifest_fingerprint(&retained)?;
    let original_fingerprint = crate::manifest::manifest_fingerprint(&content.manifest)?;
    for (original, replacement) in replacements {
        let candidate = crate::manifest::manifest_fingerprint(original)?;
        // The original event is usable only for the same semantic revision; never match names.
        if candidate != fingerprint
            && !(candidate == original_fingerprint
                && original.original_size == retained.original_size)
        {
            continue;
        }
        crate::manifest::validate_manifest(replacement)?;
        if !replacement.archive_id.starts_with("reprocess-")
            || replacement.original_size != content.size
            || replacement.shards.iter().any(|shard| {
                !allowed.contains(&shard.remote)
                    || !shard.object.starts_with(&crate::utils::remote_join(
                        &shard.remote,
                        &format!("{}/", replacement.archive_id),
                    ))
            })
        {
            bail!("completed replacement is not independently owned by the destination accounts");
        }
        return Ok(Some(replacement.clone()));
    }
    Ok(None)
}
pub(super) fn verify_replacement(
    source: &VirtualDrive,
    revision: &Revision,
    manifest: &Manifest,
    excluded: &BTreeSet<String>,
    stop: Option<&Path>,
) -> Result<String> {
    verify_replacement_with(source, revision, manifest, stop, || {
        crate::storage::reader::StorageReader::rclone_with_excluded_remotes(
            &source.rclone,
            excluded,
        )
    })
}
fn verify_replacement_with(
    source: &VirtualDrive,
    revision: &Revision,
    manifest: &Manifest,
    stop: Option<&Path>,
    mut reader_factory: impl FnMut() -> Result<crate::storage::reader::StorageReader>,
) -> Result<String> {
    for shard in &manifest.shards {
        check_stop(stop)?;
        reader_factory()?.verify(shard, true)?;
    }
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0;
    while offset < manifest.original_size {
        check_stop(stop)?;
        let count = (manifest.original_size - offset).min(1024 * 1024) as usize;
        let reader = reader_factory()?;
        let bytes = source.cache.read(
            &reader,
            manifest,
            offset,
            count,
            source.policy.workers,
            source.policy.retries,
        )?;
        if bytes.is_empty() || bytes.len() > count {
            bail!("invalid replacement read length");
        }
        hasher.update(&bytes);
        offset += bytes.len() as u64;
    }
    let hash = hasher.finalize().to_hex().to_string();
    if let Revision::Cloud { content, .. } = revision {
        if content.hash != content.manifest.content_root_blake3 && content.hash != hash {
            bail!("completed replacement differs from source file content");
        }
    }
    Ok(hash)
}

pub(crate) fn run(rclone: &str, args: &crate::cli::MountArgs) -> Result<()> {
    check_stop(args.stop_file.as_deref())?;
    let source_path = args
        .account_recovery_from
        .as_ref()
        .context("recovery source required")?;
    no_symlinks(source_path)?;
    let source_path = source_path.canonicalize()?;
    let destination = if args.workspace.exists() {
        no_symlinks(&args.workspace)?;
        args.workspace.canonicalize()?
    } else {
        let parent = args
            .workspace
            .parent()
            .context("destination parent required")?;
        no_symlink_ancestors(parent)?;
        parent.canonicalize()?.join(
            args.workspace
                .file_name()
                .context("destination name required")?,
        )
    };
    if source_path.starts_with(&destination) || destination.starts_with(&source_path) {
        bail!("recovery source and destination must be separate non-nested workspaces");
    }
    let excluded: BTreeSet<String> = args.recovery_skip_remote.iter().cloned().collect();
    let _ = crate::storage::reader::StorageReader::rclone_with_excluded_remotes(rclone, &excluded)?;
    let policy = crate::pool::load_pool_store()?
        .pools
        .get(&args.pool)
        .cloned()
        .context("unknown destination pool")?;
    for remote in &policy.remotes {
        if excluded.contains(remote.split(':').next().unwrap_or(remote)) {
            bail!("destination pool still contains an excluded account");
        }
    }
    let journal_path = destination.join("account-recovery.json");
    let bootstrap = destination.with_file_name(format!(
        ".{}.account-recovery-bootstrap.json",
        destination
            .file_name()
            .context("destination name missing")?
            .to_string_lossy()
    ));
    let resume_path = if journal_path.exists() {
        Some(journal_path.clone())
    } else if bootstrap.exists() {
        Some(bootstrap.clone())
    } else {
        None
    };
    if destination.exists() && resume_path.is_none() && fs::read_dir(&destination)?.next().is_some()
    {
        bail!("use a new empty destination workspace, or resume its recovery journal");
    }
    if bootstrap.exists() {
        no_symlinks(&bootstrap)?;
    }
    // Use a separate temporary clean cache: source clean-cache is untouched.
    // Acquire source lock and validate its saved identity before creating destination artifacts.
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
    if source.pool == args.pool {
        bail!("recovery requires a differently named destination pool");
    }
    let hashes = source_hashes(&source)?;
    let reprocess_plan = args
        .recovery_reprocess_plan
        .as_ref()
        .map(|p| p.canonicalize())
        .transpose()?;
    let replacements = if let Some(path) = &reprocess_plan {
        crate::pool::completed_reprocess_replacements(path)?
    } else {
        vec![]
    };
    let allowed = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
    let destination_policy_hash = blake3::hash(&serde_json::to_vec(&policy)?)
        .to_hex()
        .to_string();
    let mut journal = if let Some(path) = &resume_path {
        let value: Journal = crate::utils::read_json(path)?;
        if value.version != 2
            || value.source != source_path
            || value.source_hashes != hashes
            || value.destination != destination
            || value.destination_pool != args.pool
            || value.excluded != excluded
            || value.reprocess_plan != reprocess_plan
            || value.destination_policy_hash != destination_policy_hash
        {
            bail!("recovery source or configuration changed; retain both workspaces");
        }
        value
    } else {
        Journal {
            version: 2,
            source: source_path,
            source_hashes: hashes,
            destination: destination.clone(),
            approved: false,
            directories_copied: false,
            destination_policy_hash,
            reprocess_plan,
            destination_pool: args.pool.clone(),
            excluded,
            entries: BTreeMap::new(),
        }
    };
    // Sibling marker precedes workspace initialization so interrupted bootstrap is resumable.
    if resume_path.is_none() {
        durable_json(&bootstrap, &journal)?;
    }
    println!("Recovery: copying the source workspace's locally known visible files. Remote-only changes and historical versions are not migrated. Source metadata, pending writes and history are retained.");
    if !journal_path.exists() {
        // Initialize privately and publish a complete local workspace atomically.
        // An interrupted staging directory is preserved, never mistaken for the final workspace.
        if destination.exists() && fs::read_dir(&destination)?.next().is_some() {
            bail!("incomplete destination has no recovery journal; preserve it and choose a new empty destination");
        }
        let stage = tempfile::Builder::new()
            .prefix(".rpool-recovery-initialize-")
            .tempdir_in(destination.parent().context("destination parent missing")?)?
            .keep();
        let staged =
            VirtualDrive::open(rclone, &args.pool, &stage, "account-recovery", cache_limit)?;
        durable_json(&stage.join("account-recovery.json"), &journal)?;
        drop(staged); // Windows must release the workspace lock before directory rename.
        if destination.exists() {
            fs::remove_dir(&destination).context("destination is no longer empty")?;
        }
        fs::rename(&stage, &destination)
            .context("could not publish initialized recovery workspace; staging retained")?;
    }
    let mut destination_drive = VirtualDrive::open(
        rclone,
        &args.pool,
        &destination,
        "account-recovery",
        cache_limit,
    )?;
    destination_drive.spool_limit = args
        .spool_gib
        .checked_mul(1073741824)
        .context("spool limit overflow")?;
    durable_json(&journal_path, &journal)?;
    println!("Recovery: checking destination metadata");
    check_stop(args.stop_file.as_deref())?;
    destination_drive.pull()?;
    approve_destination(&destination_drive, journal.approved)?;
    if !journal.approved {
        journal.approved = true;
        durable_json(&journal_path, &journal)?;
    }
    {
        let state = destination_drive.state.lock().unwrap();
        let own: BTreeSet<_> = journal
            .entries
            .values()
            .filter_map(|entry| entry.intent.as_ref().map(|i| i.id.as_str()))
            .collect();
        if state
            .pending
            .iter()
            .any(|intent| !own.contains(intent.id.as_str()))
        {
            bail!("destination has unrelated pending writes; finish those before recovery");
        }
    }
    let view = source.view()?;
    let mut failures = 0usize;
    for (index, (path, revision)) in view.iter().enumerate() {
        check_stop(args.stop_file.as_deref())?;
        println!("Recovery file {}/{}", index + 1, view.len());
        journal
            .entries
            .entry(path.clone())
            .or_insert_with(|| Entry {
                source_revision: revision.id().into(),
                intent: None,
                complete: false,
                failed: false,
                reused: false,
                error: None,
            });
        durable_json(&journal_path, &journal)?;
        let result: Result<()> = (|| {
            let entry = journal.entries.get(path).unwrap();
            if entry.source_revision != revision.id() {
                bail!("source revision changed");
            }
            if entry.complete {
                return completion_valid(
                    &destination_drive,
                    path,
                    entry.intent.as_ref().context("completion intent missing")?,
                );
            }
            if let Some(intent) = &entry.intent {
                let state = destination_drive.state.lock().unwrap();
                if let Some(event) = state.committed_intents.get(&intent.id) {
                    if state
                        .resolved()?
                        .get(path)
                        .is_none_or(|r| &r.event_id != event)
                    {
                        bail!("destination changed; refusing recovery overwrite");
                    }
                } else if state.resolved()?.contains_key(path) {
                    bail!("destination changed; refusing recovery overwrite");
                }
            } else {
                if destination_drive.view()?.contains_key(path) {
                    bail!("destination path exists; refusing overwrite");
                }
                let intent = destination_drive.begin_intent(path, None)?;
                journal.entries.get_mut(path).unwrap().intent = Some(intent);
                durable_json(&journal_path, &journal)?;
            }
            let intent = journal.entries[path].intent.as_ref().unwrap().clone();
            let state = destination_drive.state.lock().unwrap();
            let committed = state.committed_intents.contains_key(&intent.id);
            let pending = state.pending.iter().any(|i| i.id == intent.id);
            drop(state);
            if !committed && !pending {
                if destination_drive.view()?.contains_key(path) {
                    bail!("destination changed; refusing overwrite");
                }
                if let Some(manifest) = matching_replacement(revision, &replacements, &allowed)? {
                    let hash = verify_replacement(
                        &source,
                        revision,
                        &manifest,
                        &journal.excluded,
                        args.stop_file.as_deref(),
                    )?;
                    journal.entries.get_mut(path).unwrap().reused = true;
                    durable_json(&journal_path, &journal)?;
                    check_stop(args.stop_file.as_deref())?;
                    destination_drive.commit_uploaded(
                        &intent,
                        Some(super::shared_model::Content {
                            hash,
                            size: manifest.original_size,
                            manifest,
                        }),
                    )?;
                } else {
                    copy_revision(
                        &source,
                        &destination_drive,
                        revision,
                        &destination_drive.spool_path(&intent),
                        &journal.excluded,
                        args.stop_file.as_deref(),
                    )?;
                    destination_drive.seal(intent.clone())?;
                }
            }
            check_stop(args.stop_file.as_deref())?;
            destination_drive.sync()?;
            completion_valid(&destination_drive, path, &intent)
        })();
        if let Some(entry) = journal.entries.get_mut(path) {
            entry.complete = result.is_ok();
            entry.failed = result.is_err();
            entry.error = result.as_ref().err().map(|_| "Source verification, destination conflict, or publication failed; data retained for retry".into());
        }
        if result.is_err() {
            failures += 1;
            eprintln!("Recovery file {}/{} incomplete: source unavailable or destination changed; retained for retry. See account-recovery.json.", index + 1, view.len());
        }
        durable_json(&journal_path, &journal)?;
    }
    if !journal.directories_copied {
        let mut state = destination_drive.state.lock().unwrap();
        state
            .directories
            .extend(source.state.lock().unwrap().directories.iter().cloned());
        state.save(&destination)?;
        journal.directories_copied = true;
        durable_json(&journal_path, &journal)?;
    }
    let completed = journal.entries.values().filter(|e| e.complete).count();
    let reused = journal
        .entries
        .values()
        .filter(|e| e.complete && e.reused)
        .count();
    println!("Recovery results: reused {reused} verified Reprocess replacements; copied {} files into fresh archives.", completed - reused);
    println!("Recovery copied and published {completed}/{} locally known visible files. Original workspace unchanged. Mount the destination normally to read and write on its remaining accounts.", view.len());
    if failures > 0 {
        bail!("recovery incomplete: {failures} files need attention; destination and original data retained");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::virtual_drive::fixture;
    use super::*;

    #[test]
    fn source_opener_preserves_binding_namespace_and_pending_spool() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let drive = fixture(root.path());
        {
            let mut state = drive.state.lock().unwrap();
            state.version = 6;
            state.save(root.path()).unwrap();
        }
        let intent = drive.begin_intent("draft.txt", None).unwrap();
        fs::write(drive.spool_path(&intent), b"retained local edit").unwrap();
        drive.seal(intent.clone()).unwrap();
        let binding = serde_json::json!({"version":6,"pool":"original","shared":"gone:metadata", "policy":drive.policy,
            "metadata_roots":["gone:metadata","survivor:metadata"]});
        durable_json(&root.path().join("virtual.json"), &binding).unwrap();
        let before = fs::read(root.path().join("namespace.json")).unwrap();
        let binding_before = fs::read(root.path().join("virtual.json")).unwrap();
        drop(drive);
        let source = VirtualDrive::open_recovery_source(
            "never-executed",
            root.path(),
            super::super::shard_cache::ShardCache::new(cache.path().join("cache"), 1024).unwrap(),
        )
        .unwrap();
        assert_eq!(source.pool, "original");
        assert_eq!(source.pool_sync_roots.len(), 2);
        assert!(source.view().unwrap().contains_key("draft.txt"));
        assert_eq!(
            fs::read(root.path().join("namespace.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read(root.path().join("virtual.json")).unwrap(),
            binding_before
        );
        assert_eq!(
            fs::read(source.spool_path(&intent)).unwrap(),
            b"retained local edit"
        );
    }

    #[test]
    fn local_recovery_copies_verified_bytes_and_refuses_corruption() {
        let source_root = tempfile::tempdir().unwrap();
        let destination_root = tempfile::tempdir().unwrap();
        let source = fixture(source_root.path());
        let destination = fixture(destination_root.path());
        let intent = source.begin_intent("draft.txt", None).unwrap();
        fs::write(source.spool_path(&intent), b"original bytes").unwrap();
        source.seal(intent.clone()).unwrap();
        let revision = source.view().unwrap().remove("draft.txt").unwrap();
        let next = destination.begin_intent("draft.txt", None).unwrap();
        copy_revision(
            &source,
            &destination,
            &revision,
            &destination.spool_path(&next),
            &BTreeSet::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            fs::read(destination.spool_path(&next)).unwrap(),
            b"original bytes"
        );
        assert_ne!(intent.id, next.id);
        fs::write(source.spool_path(&intent), b"corrupt bytes!").unwrap();
        assert!(copy_revision(
            &source,
            &destination,
            &revision,
            &destination.spool_path(&next),
            &BTreeSet::new(),
            None
        )
        .is_err());
    }

    #[test]
    fn recovery_honors_stop_and_spool_budget() {
        let root = tempfile::tempdir().unwrap();
        let stop = root.path().join("stop");
        assert!(check_stop(Some(&stop)).is_ok());
        fs::write(&stop, b"").unwrap();
        assert!(check_stop(Some(&stop)).is_err());
        let source = fixture(&root.path().join("source"));
        let mut destination = fixture(&root.path().join("destination"));
        destination.spool_limit = 1;
        let intent = source.begin_intent("file", None).unwrap();
        fs::write(source.spool_path(&intent), b"too large").unwrap();
        source.seal(intent).unwrap();
        let revision = source.view().unwrap().remove("file").unwrap();
        let next = destination.begin_intent("file", None).unwrap();
        assert!(copy_revision(
            &source,
            &destination,
            &revision,
            &destination.spool_path(&next),
            &BTreeSet::new(),
            None
        )
        .is_err());
    }

    fn empty_manifest(archive: &str) -> Manifest {
        Manifest {
            version: 2,
            archive_id: archive.into(),
            original_name: "file".into(),
            original_size: 0,
            shard_size: 1048576,
            created_unix: 0,
            coding: None,
            shards: vec![],
            content_root_blake3: crate::manifest::content_root_v2(0, 1048576, &None, &[]),
        }
    }

    #[test]
    fn completed_receipt_reuse_preserves_path_and_supports_import_root_identity() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let source = fixture(a.path());
        let destination = fixture(b.path());
        let original = empty_manifest("old");
        let replacement = empty_manifest("reprocess-new");
        let revision = Revision::Cloud {
            id: "unused-v6".into(),
            content: super::super::shared_model::Content {
                hash: original.content_root_blake3.clone(),
                size: 0,
                manifest: original.clone(),
            },
        };
        let matched = matching_replacement(&revision, &[(original.clone(), replacement)], &[])
            .unwrap()
            .unwrap();
        let hash =
            verify_replacement(&source, &revision, &matched, &BTreeSet::new(), None).unwrap();
        assert_eq!(hash, blake3::hash(b"").to_hex().as_str());
        let intent = destination
            .begin_intent("nested/original-name", None)
            .unwrap();
        destination
            .commit_uploaded(
                &intent,
                Some(super::super::shared_model::Content {
                    hash,
                    size: 0,
                    manifest: matched,
                }),
            )
            .unwrap();
        let view = destination.view().unwrap();
        assert!(view.contains_key("nested/original-name"));
        assert!(!view.contains_key("file"));
        let mut unrelated = original;
        unrelated.archive_id = "different-source".into();
        assert!(matching_replacement(
            &revision,
            &[(unrelated, empty_manifest("reprocess-new"))],
            &[]
        )
        .unwrap()
        .is_none());
        let wrong = Revision::Cloud {
            id: "unused".into(),
            content: super::super::shared_model::Content {
                hash: blake3::hash(b"different content").to_hex().to_string(),
                size: 0,
                manifest: empty_manifest("old"),
            },
        };
        assert!(verify_replacement(
            &source,
            &wrong,
            &empty_manifest("reprocess-new"),
            &BTreeSet::new(),
            None
        )
        .is_err());
    }

    #[test]
    fn nonempty_reprocess_reuse_verifies_bytes_without_original_account_access() {
        use crate::storage::memory::MemoryBackend;
        use crate::storage::reader::StorageReader;
        use crate::storage::reference::{BackendId, ObjectKey, ObjectRef};
        use crate::storage::registry::BackendRegistry;
        use crate::storage::traits::{OperationContext, StorageBackend, WriteOptions};
        let source_root = tempfile::tempdir().unwrap();
        let destination_root = tempfile::tempdir().unwrap();
        let source = fixture(source_root.path());
        let destination = fixture(destination_root.path());
        let bytes = b"GOOD";
        let shard = Shard {
            index: 0,
            offset: 0,
            size: 4,
            remote: "healthy:".into(),
            object: "healthy:reprocess-independent/part-0".into(),
            blake3: blake3::hash(bytes).to_hex().to_string(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        };
        let replacement = Manifest {
            version: 2,
            archive_id: "reprocess-independent".into(),
            original_name: "display-name".into(),
            original_size: 4,
            shard_size: 4,
            created_unix: 0,
            coding: None,
            content_root_blake3: crate::manifest::content_root_v2(
                4,
                4,
                &None,
                std::slice::from_ref(&shard),
            ),
            shards: vec![shard.clone()],
        };
        crate::manifest::validate_manifest(&replacement).unwrap();
        let mut original = replacement.clone();
        original.archive_id = "original".into();
        original.shards[0].remote = "removed-account:".into();
        original.shards[0].object = "removed-account:original/part-0".into();
        original.content_root_blake3 =
            crate::manifest::content_root_v2(4, 4, &None, &original.shards);
        let imported = Revision::Cloud {
            id: "source-event".into(),
            content: super::super::shared_model::Content {
                hash: original.content_root_blake3.clone(),
                size: 4,
                manifest: original.clone(),
            },
        };
        let matched = matching_replacement(
            &imported,
            &[(original.clone(), replacement.clone())],
            &["healthy:".into()],
        )
        .unwrap()
        .unwrap();
        let backend_id = BackendId::new("recovery-replacement-only").unwrap();
        let backend = Arc::new(MemoryBackend::new(backend_id.clone()));
        let key = ObjectKey::new("replacement").unwrap();
        backend
            .write(
                &OperationContext::none(),
                &key,
                &mut std::io::Cursor::new(bytes),
                &WriteOptions::default(),
            )
            .unwrap();
        let factory = || -> Result<StorageReader> {
            let mut registry = BackendRegistry::new();
            registry.register(backend.clone())?;
            // No original-account route exists: any source read would fail this test.
            let bindings = BTreeMap::from([(
                shard.object.clone(),
                ObjectRef::new(backend_id.clone(), key.clone()),
            )]);
            Ok(StorageReader::from_registry(
                registry,
                bindings,
                OperationContext::none(),
            ))
        };
        let hash = verify_replacement_with(&source, &imported, &matched, None, factory).unwrap();
        assert_eq!(hash, blake3::hash(bytes).to_hex().as_str());
        let wrong_same_size = Revision::Cloud {
            id: "different-event".into(),
            content: super::super::shared_model::Content {
                hash: blake3::hash(b"EVIL").to_hex().to_string(),
                size: 4,
                manifest: original,
            },
        };
        assert!(
            verify_replacement_with(&source, &wrong_same_size, &matched, None, factory).is_err()
        );
        let intent = destination
            .begin_intent("folder/retained-name", None)
            .unwrap();
        destination
            .commit_uploaded(
                &intent,
                Some(super::super::shared_model::Content {
                    hash,
                    size: 4,
                    manifest: matched,
                }),
            )
            .unwrap();
        let view = destination.view().unwrap();
        let Revision::Cloud { content, .. } = &view["folder/retained-name"] else {
            panic!("expected adopted cloud revision")
        };
        assert_eq!(content.manifest.archive_id, "reprocess-independent");
        assert!(!destination.root.join("owned-archives.json").exists());
        assert!(!source.root.join("owned-archives.json").exists());
        assert!(!destination.spool_path(&intent).exists());
    }

    #[test]
    fn completion_revalidation_rejects_modified_and_deleted_destination() {
        let root = tempfile::tempdir().unwrap();
        let mut drive = fixture(root.path());
        drive.pool_sync_roots = vec!["fixture:metadata".into()];
        drive.state.lock().unwrap().version = 6;
        let intent = drive.begin_intent("file", None).unwrap();
        let content = super::super::shared_model::Content {
            hash: blake3::hash(b"").to_hex().to_string(),
            size: 0,
            manifest: empty_manifest("reprocess-new"),
        };
        drive
            .commit_uploaded(&intent, Some(content.clone()))
            .unwrap();
        let event = drive.state.lock().unwrap().committed_intents[&intent.id].clone();
        drive.state.lock().unwrap().published.insert(event.clone());
        completion_valid(&drive, "file", &intent).unwrap();
        let visible = drive.view().unwrap()["file"].clone();
        // A sequential editor reads the prior revision first. Without this,
        // the conservative empty baseline plus identical content creates the
        // same content-addressed event, not a modification.
        drive.pin_read("file", &visible).unwrap();
        let changed = drive.begin_intent("file", Some(&visible)).unwrap();
        assert_eq!(changed.parents, vec![event.clone()]);
        drive.commit_uploaded(&changed, Some(content)).unwrap();
        assert_ne!(
            drive.state.lock().unwrap().committed_intents[&changed.id],
            event
        );
        assert!(completion_valid(&drive, "file", &intent).is_err());
        drive.delete("file").unwrap();
        let deletion = drive.state.lock().unwrap().pending.last().unwrap().clone();
        drive.commit_uploaded(&deletion, None).unwrap();
        assert!(!drive.view().unwrap().contains_key("file"));
        assert!(completion_valid(&drive, "file", &intent).is_err());
    }

    #[test]
    fn unapproved_nonempty_destination_remains_rejected_on_retry() {
        let root = tempfile::tempdir().unwrap();
        let drive = fixture(root.path());
        approve_destination(&drive, false).unwrap();
        drive
            .state
            .lock()
            .unwrap()
            .directories
            .insert("existing".into());
        assert!(approve_destination(&drive, false).is_err());
        assert!(approve_destination(&drive, false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlink_workspace_entries() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        assert!(no_symlinks(root.path()).is_err());
    }
}
