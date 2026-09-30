//! Exact-object, opt-in maintenance. Shared histories are never swept by this protocol.
use super::namespace::{durable_json, Intent};
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

/// Local write spool budget refusal. Existing writes are retained.
#[derive(Debug)]
pub(crate) struct SpoolBudgetExceeded;
impl std::fmt::Display for SpoolBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("local write spool budget exceeded; existing writes retained. Sync/recover writes or increase --spool-gib")
    }
}
impl std::error::Error for SpoolBudgetExceeded {}

#[derive(Serialize, Deserialize)]
struct Checked<T> {
    hash: String,
    payload: T,
}
fn write_checked<T: Serialize>(path: &Path, payload: &T) -> Result<()> {
    durable_json(
        path,
        &Checked {
            hash: blake3::hash(&serde_json::to_vec(payload)?)
                .to_hex()
                .to_string(),
            payload,
        },
    )
}
fn read_checked<T: Serialize + serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let envelope: Checked<T> = crate::utils::read_json(path)?;
    if envelope.hash
        != blake3::hash(&serde_json::to_vec(&envelope.payload)?)
            .to_hex()
            .as_str()
    {
        bail!("retention metadata checksum mismatch; preserve state and recover explicitly");
    }
    Ok(envelope.payload)
}

pub(super) fn real_tree(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            bail!("retention refuses reparse points");
        }
    }
    if meta.file_type().is_symlink() || (!meta.is_dir() && !meta.is_file()) {
        bail!("retention refuses links and special files");
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            real_tree(&entry?.path())?;
        }
    }
    Ok(())
}

impl VirtualDrive {
    /// A checkpoint and publication are prerequisites. A revision lease covers
    /// DAV handles, read tasks, pinned views and in-progress rename/hydration.
    pub(crate) fn cleanup_committed_spool(&self) -> Result<u64> {
        if self.bounded_shared {
            return self.cleanup_checkpoint_spool();
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("namespace lock poisoned"))?;
        // Advance the previous recovery checkpoint to the same committed roots
        // before deleting anything. A failed checkpoint means no cleanup.
        state.save(&self.root)?;
        let mut leases = self.local_leases.lock().unwrap();
        let spool = self.root.join("spool");
        let mut removed = 0u64;
        for entry in fs::read_dir(&spool)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.len() != 64
                || !id.bytes().all(|c| c.is_ascii_hexdigit())
                || state.pending.iter().any(|i| i.id == id)
                || leases.get(&id).is_some_and(|l| l.strong_count() > 0)
            {
                continue;
            }
            let Some(event_id) = state.committed_intents.get(&id) else {
                continue;
            };
            if self.shared_root.is_some() && !state.published.contains(event_id) {
                continue;
            }
            // Metadata-only delete/MOVE intents create empty directories too.
            // Only a proven committed identity can authorize their removal.
            if real_tree(&entry.path()).is_err() {
                continue;
            }
            if fs::read_dir(entry.path())?.next().is_none() {
                fs::remove_dir(entry.path())?;
                continue;
            }
            let Some(content) = state.events.get(event_id).and_then(|e| e.content.as_ref()) else {
                continue;
            };
            // Unknown, incomplete, or corrupt data remains recoverable, never garbage.
            let Ok(intent) = crate::utils::read_json::<Intent>(&entry.path().join("intent.json"))
            else {
                continue;
            };
            if intent.id != id
                || intent.spool.as_deref() != Some(id.as_str())
                || intent.size != content.size
                || intent.hash != content.hash
            {
                continue;
            }
            let source = entry.path().join("content");
            if !fs::metadata(&source).is_ok_and(|m| m.len() == intent.size)
                || !crate::utils::hash_file_range(&source, 0, intent.size)
                    .is_ok_and(|h| h == intent.hash)
            {
                continue;
            }
            fs::remove_dir_all(entry.path())?;
            removed = removed.saturating_add(intent.size);
        }
        leases.retain(|_, lease| lease.strong_count() > 0);
        #[cfg(unix)]
        File::open(spool)?.sync_all()?;
        Ok(removed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnedArchive {
    path: String,
    sequence: u64,
    manifest: Manifest,
    manifest_objects: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnedStore {
    version: u32,
    archives: BTreeMap<String, OwnedArchive>,
}
impl Default for OwnedStore {
    fn default() -> Self {
        Self {
            version: 1,
            archives: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RetentionReport {
    pub shared: bool,
    pub keep_previous: usize,
    pub tracked_archives: usize,
    pub obsolete_archives: usize,
    pub reclaimable_bytes: u64,
    pub objects: Vec<String>,
    pub note: String,
    fingerprints: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct GcJournal {
    version: u32,
    report: RetentionReport,
    remaining: Vec<String>,
    next: super::namespace::Namespace,
}

fn objects(archive: &OwnedArchive) -> impl Iterator<Item = &String> {
    archive
        .manifest
        .shards
        .iter()
        .map(|s| &s.object)
        .chain(archive.manifest_objects.iter())
}
fn load_owned(root: &Path) -> Result<OwnedStore> {
    let path = root.join("owned-archives.json");
    let store: OwnedStore = if path.exists() {
        read_checked(&path)?
    } else {
        OwnedStore::default()
    };
    if store.version != 1 {
        bail!("unsupported ownership registry");
    }
    for (fingerprint, archive) in &store.archives {
        super::namespace::valid_path(&archive.path)?;
        crate::manifest::validate_manifest(&archive.manifest)?;
        if *fingerprint != crate::manifest::manifest_fingerprint(&archive.manifest)? {
            bail!("ownership fingerprint mismatch");
        }
        // Locally recorded exact archive objects only; never recursive prefix purge.
        let id = &archive.manifest.archive_id;
        let suffix = id
            .strip_prefix("virtual-")
            .context("unowned archive identity")?;
        if suffix.len() != 64 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("unowned archive identity");
        }
        for object in objects(archive) {
            let (_, path) = object.split_once(':').context("non-remote owned object")?;
            let components: Vec<_> = path.split('/').collect();
            if !components.contains(&id.as_str())
                || components.iter().any(|c| matches!(*c, "." | ".."))
            {
                bail!("owned object outside its archive");
            }
        }
    }
    Ok(store)
}
impl VirtualDrive {
    pub(crate) fn record_owned_archive(
        &self,
        intent: &Intent,
        manifest: &Manifest,
        remotes: &[String],
    ) -> Result<()> {
        if manifest.archive_id != format!("virtual-{}", intent.id)
            || manifest.original_size != intent.size
        {
            bail!("verified upload ownership mismatch");
        }
        let mut store = load_owned(&self.root)?;
        let fingerprint = crate::manifest::manifest_fingerprint(manifest)?;
        let sequence = store
            .archives
            .get(&fingerprint)
            .map(|a| a.sequence)
            .unwrap_or(
                store
                    .archives
                    .values()
                    .map(|a| a.sequence)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .context("ownership sequence overflow")?,
            );
        let archive = OwnedArchive {
            path: intent.event_path.clone(),
            sequence,
            manifest: manifest.clone(),
            manifest_objects: remotes
                .iter()
                .map(|remote| {
                    crate::utils::remote_join(
                        remote,
                        &format!("{}/manifest.json", manifest.archive_id),
                    )
                })
                .collect(),
        };
        store.archives.insert(fingerprint, archive);
        write_checked(&self.root.join("owned-archives.json"), &store)
    }

    pub(crate) fn retention_report(&self, keep: usize) -> Result<RetentionReport> {
        let store = load_owned(&self.root)?;
        let state = self.state.lock().unwrap();
        let mut protected = BTreeSet::new();
        for resolved in state.resolved()?.values() {
            if let Some(c) = &resolved.event.content {
                protected.insert(crate::manifest::manifest_fingerprint(&c.manifest)?);
            }
        }
        let mut histories = BTreeMap::<String, Vec<(u64, String)>>::new();
        for (fp, a) in &store.archives {
            if !protected.contains(fp) {
                histories
                    .entry(a.path.clone())
                    .or_default()
                    .push((a.sequence, fp.clone()));
            }
        }
        for history in histories.values_mut() {
            history.sort();
            protected.extend(history.iter().rev().take(keep).map(|(_, fp)| fp.clone()));
        }
        // Different fingerprints/remote aliases of one archive are not
        // independent ownership units. An import protects the entire identity.
        let mut protected_ids = BTreeSet::new();
        for (fp, archive) in &store.archives {
            if protected.contains(fp) {
                protected_ids.insert(archive.manifest.archive_id.clone());
            }
        }
        for event in state.events.values() {
            if let Some(c) = &event.content {
                if !store
                    .archives
                    .contains_key(&crate::manifest::manifest_fingerprint(&c.manifest)?)
                {
                    protected_ids.insert(c.manifest.archive_id.clone());
                }
            }
        }
        protected.extend(
            store
                .archives
                .iter()
                .filter(|(_, a)| protected_ids.contains(&a.manifest.archive_id))
                .map(|(fp, _)| fp.clone()),
        );
        let candidates: BTreeSet<_> = store
            .archives
            .keys()
            .filter(|fp| !protected.contains(*fp))
            .cloned()
            .collect();
        let mut protected_objects = BTreeSet::new();
        for (fp, a) in &store.archives {
            if !candidates.contains(fp) {
                protected_objects.extend(objects(a).cloned());
            }
        }
        // Imported/legacy content never grants ownership; overlapping addresses stay live.
        for event in state.events.values() {
            if let Some(c) = &event.content {
                if !candidates.contains(&crate::manifest::manifest_fingerprint(&c.manifest)?) {
                    protected_objects.extend(c.manifest.shards.iter().map(|s| s.object.clone()));
                }
            }
        }
        let mut deletion = BTreeMap::<String, u64>::new();
        for fp in &candidates {
            let a = &store.archives[fp];
            for shard in &a.manifest.shards {
                if !protected_objects.contains(&shard.object) {
                    deletion.insert(shard.object.clone(), shard.size);
                }
            }
            for object in &a.manifest_objects {
                if !protected_objects.contains(object) {
                    deletion.entry(object.clone()).or_insert(0);
                }
            }
        }
        Ok(RetentionReport {
            shared: self.shared_root.is_some(), keep_previous: keep,
            tracked_archives: store.archives.len(), obsolete_archives: candidates.len(),
            reclaimable_bytes: deletion.values().try_fold(0u64, |sum, size| sum.checked_add(*size).context("retention byte count overflow"))?, objects: deletion.into_keys().collect(),
            fingerprints: candidates.into_iter().collect(),
            note: "Preview only. Apply requires an unshared, unmounted, exclusively owned workspace, no pending/partial writes or external archive references. Imported/legacy/untracked archives and failed upload objects are never swept. Shared GC is blocked until a fenced epoch protocol exists. Backend trash/versioning may delay quota reclamation.".into(),
        })
    }

    /// Explicit offline operation only. The journal blocks ordinary use until
    /// exact-object deletion and the new local checkpoint have both completed.
    pub(crate) fn apply_retention(&self, keep: usize, exclusive: bool) -> Result<RetentionReport> {
        if self.shared_root.is_some() || !exclusive {
            bail!("remote retention requires an unshared, exclusively owned workspace");
        }
        let storage =
            crate::storage::writer::StorageWriter::for_pool(&self.rclone, self.policy.native_crypt);
        // Verify retained versions before sacrificing any historical fallback.
        let journal_path = self.root.join("retention-journal.json");
        let report = if journal_path.exists() {
            read_checked::<GcJournal>(&journal_path)?.report
        } else {
            self.retention_report(keep)?
        };
        let mut retained = BTreeMap::new();
        for (fp, archive) in load_owned(&self.root)?.archives {
            if !report.fingerprints.contains(&fp) {
                retained.insert(fp, archive.manifest);
            }
        }
        for resolved in self.state.lock().unwrap().resolved()?.values() {
            if let Some(c) = &resolved.event.content {
                retained.insert(
                    crate::manifest::manifest_fingerprint(&c.manifest)?,
                    c.manifest.clone(),
                );
            }
        }
        for manifest in retained.values() {
            let temp = tempfile::NamedTempFile::new()?;
            serde_json::to_writer(temp.as_file(), manifest)?;
            crate::commands::verify_with_storage(
                storage.reader(),
                &temp.path().to_string_lossy(),
                true,
                self.policy.workers,
            )?;
        }
        self.apply_retention_with(keep, exclusive, |object| {
            storage.ensure_destination(object)?;
            match storage.delete(object) {
                Ok(()) => Ok(()),
                Err(e)
                    if e.downcast_ref::<crate::storage::error::StorageError>()
                        .is_some_and(|e| {
                            e.kind() == crate::storage::error::StorageErrorKind::NotFound
                        }) =>
                {
                    Ok(())
                }
                Err(e) => Err(e),
            }
        })
    }
    fn apply_retention_with(
        &self,
        keep: usize,
        exclusive: bool,
        mut delete: impl FnMut(&str) -> Result<()>,
    ) -> Result<RetentionReport> {
        if self.shared_root.is_some() || !exclusive {
            bail!("remote retention requires an unshared workspace and --exclusive-archive-ownership; shared/offline clients cannot be fenced by this protocol");
        }
        super::adapter::preflight_virtual(&self.root)?;
        fn cache_has_files(path: &Path) -> Result<bool> {
            let meta = fs::symlink_metadata(path)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Ok(true);
            }
            for entry in fs::read_dir(path)? {
                if cache_has_files(&entry?.path())? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        let cache = self.root.join("vfs-cache");
        if cache.exists() && cache_has_files(&cache)? {
            bail!("persistent VFS cache remains; replay/drain it using the original mount before retention (never delete cache to bypass this check)");
        }
        let _gate = self.sync_gate.lock().unwrap();
        if !self.pins.lock().unwrap().is_empty()
            || self
                .local_leases
                .lock()
                .unwrap()
                .values()
                .any(|l| l.strong_count() > 0)
            || !self.state.lock().unwrap().pending.is_empty()
        {
            bail!("stop all readers and sync pending writes before retention");
        }
        self.cleanup_committed_spool()?;
        if fs::read_dir(self.root.join("spool"))?.next().is_some() {
            bail!(
                "unrecognized/partial spool remains; recover and reconcile before remote retention"
            );
        }
        let journal_path = self.root.join("retention-journal.json");
        let mut journal: GcJournal = if journal_path.exists() {
            read_checked(&journal_path)?
        } else {
            let report = self.retention_report(keep)?;
            let mut next = self.state.lock().unwrap().clone();
            let resolved = next.resolved()?;
            // Sole-writer quiescent checkpoint. Keep every concurrent visible
            // version at its resolved filename, but no obsolete ancestry graph.
            next.version = 4; // Old binaries must fail closed after destructive maintenance.
            next.events.clear();
            for (path, resolved) in resolved {
                let mut event = resolved.event;
                event.path = path;
                event.parents.clear();
                next.events.insert(event.id()?, event);
            }
            next.published.clear();
            next.bases.clear();
            next.committed_intents.clear();
            next.validate()?;
            let journal = GcJournal {
                version: 1,
                remaining: report.objects.clone(),
                report,
                next,
            };
            write_checked(&journal_path, &journal)?;
            journal
        };
        if journal.version != 1 {
            bail!("unsupported retention journal");
        }
        journal.next.validate()?;
        if journal.next.device != self.state.lock().unwrap().device || journal.next.version != 4 {
            bail!("retention journal workspace/version mismatch");
        }
        let store = load_owned(&self.root)?;
        let authorized: BTreeSet<_> = journal
            .report
            .fingerprints
            .iter()
            .filter_map(|fp| store.archives.get(fp))
            .flat_map(objects)
            .cloned()
            .collect();
        if journal.remaining.iter().any(|o| !authorized.contains(o)) {
            bail!("retention journal contains an object outside the ownership registry");
        }
        // Fence older binaries before the first irreversible object deletion.
        // Preserve full pre-sweep state for this binary's recovery, but both
        // primary and fallback must reject legacy v3 readers.
        {
            let mut state = self.state.lock().unwrap();
            if state.version != 4 {
                state.version = 4;
                state.save(&self.root)?;
                state.save(&self.root)?;
            }
        }
        while let Some(object) = journal.remaining.last().cloned() {
            delete(&object)?;
            journal.remaining.pop();
            write_checked(&journal_path, &journal)?;
        }
        let mut state = self.state.lock().unwrap();
        journal.next.save(&self.root)?;
        *state = journal.next.clone();
        // Advance fallback checkpoint too; it must not reference swept history.
        state.save(&self.root)?;
        drop(state);
        let mut store = store;
        for fp in &journal.report.fingerprints {
            store.archives.remove(fp);
        }
        write_checked(&self.root.join("owned-archives.json"), &store)?;
        fs::remove_file(&journal_path)?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(journal.report)
    }
}

impl VirtualDrive {
    pub(crate) fn spool_bytes(&self) -> Result<u64> {
        let mut sum = 0u64;
        for entry in fs::read_dir(self.root.join("spool"))? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                bail!("invalid spool directory");
            }
            let path = entry.path().join("content");
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    sum = sum.checked_add(meta.len()).context("spool size overflow")?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => bail!("invalid spool content"),
            }
        }
        Ok(sum)
    }
    pub(crate) fn copy_to_spool(&self, source: &Path, target: &Path) -> Result<()> {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            self.write_spool_bytes(&mut output, &buffer[..n])?;
        }
        output.sync_all()?;
        Ok(())
    }
    fn admit_spool_growth(&self, file: &File, end: u64) -> Result<()> {
        let growth = end.saturating_sub(file.metadata()?.len());
        if growth > 0
            && self
                .spool_bytes()?
                .checked_add(growth)
                .is_none_or(|n| n > self.spool_limit)
        {
            return Err(SpoolBudgetExceeded.into());
        }
        Ok(())
    }
    pub(crate) fn write_spool_bytes(&self, file: &mut File, bytes: &[u8]) -> Result<()> {
        let _gate = self.spool_writes.lock().unwrap();
        let end = file
            .stream_position()?
            .checked_add(bytes.len() as u64)
            .context("write range overflow")?;
        self.admit_spool_growth(file, end)?;
        if super::crash::armed("spool.partial_write") {
            file.write_all(&bytes[..bytes.len() / 2])?;
            super::crash::point("spool.partial_write")?;
        }
        file.write_all(bytes)?;
        Ok(())
    }
    /// Resize an unsealed spool file under the same budget as writes.
    pub(crate) fn resize_spool(&self, file: &File, len: u64) -> Result<()> {
        let _gate = self.spool_writes.lock().unwrap();
        self.admit_spool_growth(file, len)?;
        file.set_len(len)?;
        Ok(())
    }
    /// Remove the spool of an intent that was begun but never sealed. Refuses a
    /// spool that has an intent record or is pending, because that data may be
    /// acknowledged.
    pub(crate) fn discard_unsealed(&self, intent: &Intent) -> Result<()> {
        let id = intent.spool.as_deref().context("intent has no spool")?;
        let dir = self.root.join("spool").join(id);
        if dir.join("intent.json").exists()
            || self
                .state
                .lock()
                .map_err(|_| anyhow!("namespace lock poisoned"))?
                .pending
                .iter()
                .any(|i| i.id == intent.id)
        {
            bail!("refusing to discard a sealed spool");
        }
        match fs::remove_dir_all(&dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{shared_model::Content, virtual_drive::fixture};
    use super::*;
    fn write(d: &VirtualDrive, bytes: &[u8]) -> Intent {
        // A newly observed, sequential edit, not an unresolved concurrent editor.
        d.state.lock().unwrap().bases.clear();
        let i = d.begin("file").unwrap();
        fs::write(d.spool_path(&i), bytes).unwrap();
        d.seal(i).unwrap();
        d.state.lock().unwrap().pending.last().unwrap().clone()
    }
    fn commit(d: &VirtualDrive, i: &Intent) -> String {
        let id = format!("virtual-{}", i.id);
        let shards = vec![Shard {
            index: 0,
            offset: 0,
            size: i.size,
            remote: "crypt:".into(),
            object: format!("crypt:{id}/shards/00000000.bin"),
            blake3: i.hash.clone(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        }];
        let manifest = Manifest {
            version: 1,
            archive_id: id,
            original_name: "file".into(),
            original_size: i.size,
            shard_size: 1024,
            created_unix: d.state.lock().unwrap().events.len() as u64,
            content_root_blake3: crate::manifest::content_root_v1(&shards),
            coding: None,
            shards,
        };
        d.record_owned_archive(i, &manifest, &["crypt:".into()])
            .unwrap();
        let fp = crate::manifest::manifest_fingerprint(&manifest).unwrap();
        d.commit_uploaded(
            i,
            Some(Content {
                hash: i.hash.clone(),
                size: i.size,
                manifest,
            }),
        )
        .unwrap();
        fp
    }
    #[test]
    fn cleanup_waits_for_readers_publication_and_preserves_unknown_writes() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.shared_root = Some("crypt:shared".into());
        let i = write(&d, b"one");
        let reader = d.view().unwrap()["file"].clone();
        commit(&d, &i);
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        let event_id = d.state.lock().unwrap().committed_intents[&i.id].clone();
        d.state.lock().unwrap().published.insert(event_id);
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        assert_eq!(d.read(&reader, 0, 3).unwrap(), b"one");
        drop(reader);
        let unknown = d.begin("partial").unwrap();
        fs::write(d.spool_path(&unknown), b"partial").unwrap();
        assert_eq!(d.cleanup_committed_spool().unwrap(), 3);
        assert!(!d.spool_path(&i).exists());
        assert!(d.spool_path(&unknown).exists());
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        let previous: serde_json::Value =
            crate::utils::read_json(&temp.path().join("namespace.previous.json")).unwrap();
        assert!(previous["payload"]["pending"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn cleanup_preserves_pending_corrupt_and_writer_leased_content() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let i = write(&d, b"one");
        let lease = d.local_lease(&i.id);
        commit(&d, &i);
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        drop(lease);
        fs::write(d.spool_path(&i), b"bad").unwrap();
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        let pending = write(&d, b"next");
        assert_eq!(d.cleanup_committed_spool().unwrap(), 0);
        assert!(d.spool_path(&pending).exists());
    }
    #[test]
    fn metadata_only_committed_directories_are_reclaimed() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let i = write(&d, b"one");
        commit(&d, &i);
        d.rename_file("file", "new").unwrap();
        d.delete("new").unwrap();
        let tombstone = d.state.lock().unwrap().pending.last().unwrap().clone();
        d.commit_uploaded(&tombstone, None).unwrap();
        d.pins.lock().unwrap().clear();
        d.cleanup_committed_spool().unwrap();
        assert_eq!(fs::read_dir(temp.path().join("spool")).unwrap().count(), 0);
    }
    #[test]
    fn remote_retention_is_opt_in_exact_resumable_and_compacts_only_after_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let first = write(&d, b"one");
        let old_fp = commit(&d, &first);
        let second = write(&d, b"two");
        let live_fp = commit(&d, &second);
        d.pins.lock().unwrap().clear();
        let preview = d.retention_report(0).unwrap();
        assert_eq!(preview.obsolete_archives, 1);
        assert_eq!(preview.reclaimable_bytes, 3);
        assert_eq!(preview.objects.len(), 2);
        assert!(preview.objects.iter().all(|o| o.contains(&first.id)));
        assert!(d
            .apply_retention_with(0, false, |_| panic!("unauthorized delete"))
            .is_err());
        let mut count = 0;
        assert!(d
            .apply_retention_with(0, true, |_| {
                count += 1;
                if count == 2 {
                    bail!("injected failure");
                }
                Ok(())
            })
            .is_err());
        assert!(temp.path().join("retention-journal.json").exists());
        assert!(d.sync().unwrap_err().to_string().contains("retention"));
        assert_eq!(d.state.lock().unwrap().events.len(), 2);
        let mut retried = vec![];
        d.apply_retention_with(99, true, |o| {
            retried.push(o.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(retried.len(), 1);
        assert!(!temp.path().join("retention-journal.json").exists());
        assert_eq!(d.state.lock().unwrap().events.len(), 1);
        let owned = load_owned(temp.path()).unwrap();
        assert!(!owned.archives.contains_key(&old_fp));
        assert!(owned.archives.contains_key(&live_fp));
        assert_eq!(d.state.lock().unwrap().logical_used().unwrap(), 3);
    }
    #[test]
    fn shared_retention_cannot_delete_and_keep_policy_preserves_history() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        for bytes in [b"one", b"two", b"tri"] {
            let i = write(&d, bytes);
            commit(&d, &i);
        }
        assert_eq!(d.retention_report(1).unwrap().obsolete_archives, 1);
        d.shared_root = Some("crypt:shared".into());
        assert!(d
            .apply_retention_with(0, true, |_| panic!("shared deletion"))
            .is_err());
    }
    #[test]
    fn retention_replays_final_persistence_boundaries_without_more_deletes() {
        for boundary in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let d = fixture(temp.path());
            let i = write(&d, b"old");
            commit(&d, &i);
            let i = write(&d, b"new");
            commit(&d, &i);
            d.pins.lock().unwrap().clear();
            assert!(d
                .apply_retention_with(0, true, |_| bail!("stop before first delete"))
                .is_err());
            let path = temp.path().join("retention-journal.json");
            let mut journal: GcJournal = read_checked(&path).unwrap();
            journal.remaining.clear(); // All exact objects deleted, durable receipt persisted.
            write_checked(&path, &journal).unwrap();
            if boundary >= 1 {
                journal.next.save(temp.path()).unwrap();
                *d.state.lock().unwrap() = journal.next.clone();
            }
            if boundary >= 2 {
                let mut owned = load_owned(temp.path()).unwrap();
                for fp in &journal.report.fingerprints {
                    owned.archives.remove(fp);
                }
                write_checked(&temp.path().join("owned-archives.json"), &owned).unwrap();
            }
            d.apply_retention_with(0, true, |_| panic!("deletion already finished"))
                .unwrap();
            assert!(!path.exists());
            assert_eq!(load_owned(temp.path()).unwrap().archives.len(), 1);
            for name in ["namespace.json", "namespace.previous.json"] {
                let value: serde_json::Value =
                    crate::utils::read_json(&temp.path().join(name)).unwrap();
                assert_eq!(value["payload"]["version"], 4);
                assert_eq!(value["payload"]["events"].as_object().unwrap().len(), 1);
            }
        }
    }
    #[test]
    fn imported_alias_of_owned_archive_protects_the_archive_identity() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let i = write(&d, b"old");
        commit(&d, &i);
        let original = d
            .state
            .lock()
            .unwrap()
            .events
            .values()
            .next()
            .unwrap()
            .clone();
        let i = write(&d, b"new");
        commit(&d, &i);
        let mut imported = original;
        imported.path = "imported".into();
        imported.parents.clear();
        let manifest = &mut imported.content.as_mut().unwrap().manifest;
        manifest.shards[0].remote = "alias:".into();
        manifest.shards[0].object = manifest.shards[0].object.replacen("crypt:", "alias:", 1);
        d.state
            .lock()
            .unwrap()
            .events
            .insert(imported.id().unwrap(), imported);
        assert_eq!(d.retention_report(0).unwrap().obsolete_archives, 0);
    }
    #[test]
    fn retention_refuses_detached_vfs_cache_and_local_move_obeys_spool_limit() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.spool_limit = 5;
        let i = write(&d, b"four");
        assert!(d.rename_file("file", "moved").is_err());
        assert_eq!(fs::read(d.spool_path(&i)).unwrap(), b"four");
        assert!(d.view().unwrap().contains_key("file"));
        assert!(!d.view().unwrap().contains_key("moved"));
        fs::create_dir(temp.path().join("vfs-cache")).unwrap();
        fs::write(temp.path().join("vfs-cache/dirty"), b"edit").unwrap();
        assert!(d
            .apply_retention_with(0, true, |_| panic!("must preserve cache"))
            .unwrap_err()
            .to_string()
            .contains("VFS cache"));
    }
    #[test]
    fn corrupt_retention_registry_never_authorizes_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let i = write(&d, b"old");
        commit(&d, &i);
        let path = temp.path().join("owned-archives.json");
        let mut value: serde_json::Value = crate::utils::read_json(&path).unwrap();
        value["hash"] = serde_json::json!("broken");
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(d.retention_report(0).is_err());
    }
    #[test]
    fn spool_budget_counts_partial_writes_and_rejects_growth_before_write() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.spool_limit = 5;
        let i = d.begin("partial").unwrap();
        let mut f = File::create(d.spool_path(&i)).unwrap();
        d.write_spool_bytes(&mut f, b"hello").unwrap();
        assert!(d.write_spool_bytes(&mut f, b"!").is_err());
        assert_eq!(fs::read(d.spool_path(&i)).unwrap(), b"hello");
        f.seek(SeekFrom::Start(0)).unwrap();
        d.write_spool_bytes(&mut f, b"H").unwrap();
        assert_eq!(d.spool_bytes().unwrap(), 5);
    }
}
