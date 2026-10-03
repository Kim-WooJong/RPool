//! Local write spool: budget, growth and cleanup of committed, published writes.
use super::namespace::Intent;
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

mod identity;
mod meter;
pub(crate) use meter::SpoolMeter;

/// Local write spool budget refusal. Existing writes are retained.
#[derive(Debug)]
pub(crate) struct SpoolBudgetExceeded;
impl std::fmt::Display for SpoolBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("local write spool budget exceeded; existing writes retained. Sync/recover writes or increase --spool-gib")
    }
}
impl std::error::Error for SpoolBudgetExceeded {}

/// Fails if `path` or anything below it is a symlink, reparse point
/// (Windows) or special file; checked before recursive removal by
/// `cache_recovery`.
pub(super) fn real_tree(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            bail!("spool cleanup refuses reparse points");
        }
    }
    if meta.file_type().is_symlink() || (!meta.is_dir() && !meta.is_file()) {
        bail!("spool cleanup refuses links and special files");
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
            if !self.pool_sync_roots.is_empty() && !state.published.contains(event_id) {
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
        if removed > 0 {
            self.spool_writes.lock().unwrap().invalidate();
        }
        #[cfg(unix)]
        File::open(spool)?.sync_all()?;
        Ok(removed)
    }
}

impl VirtualDrive {
    /// Bytes of every spool image, including images staged for a MOVE. An
    /// image hard-linked into several spool directories counts once.
    pub(crate) fn spool_bytes(&self) -> Result<u64> {
        let mut sum = 0u64;
        let mut linked = BTreeSet::new();
        let mut count = |path: PathBuf| -> Result<()> {
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    if identity::shared_identity(&path, &meta)?.is_none_or(|id| linked.insert(id)) {
                        sum = sum.checked_add(meta.len()).context("spool size overflow")?;
                    }
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => bail!("invalid spool content"),
            }
        };
        for entry in fs::read_dir(self.root.join("spool"))? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                bail!("invalid spool directory");
            }
            count(entry.path().join("content"))?;
            let moving = entry.path().join("moving");
            match fs::symlink_metadata(&moving) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    for staged in fs::read_dir(&moving)? {
                        count(staged?.path().join("content"))?;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => bail!("invalid spool directory"),
            }
        }
        Ok(sum)
    }
    /// Copies `source` into a new spool image `target` through
    /// `write_spool_bytes` (budgeted) and flushes it. Used by rename when a
    /// file must be materialized into the spool.
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
    /// Admit growth of `file` to `end` under the spool budget (the caller
    /// holds the `spool_writes` gate, whose meter replaces a per-write scan).
    /// Growth first makes disk room by evicting old clean cache entries.
    fn admit_spool_growth(&self, meter: &mut SpoolMeter, file: &File, end: u64) -> Result<()> {
        let growth = end.saturating_sub(file.metadata()?.len());
        if !meter.admit(growth, self.spool_limit, &|| self.spool_bytes())? {
            return Err(SpoolBudgetExceeded.into());
        }
        if growth > 0 {
            // Best effort: the write itself reports a genuinely full disk.
            let _ = self.cache.relieve_disk(growth);
        }
        Ok(())
    }
    /// Writes `bytes` at the file's current position after admitting the
    /// growth under the spool budget (`SpoolBudgetExceeded` when it does not fit).
    /// Every spool write (DAV, native, import, recovery) goes through here.
    pub(crate) fn write_spool_bytes(&self, file: &mut File, bytes: &[u8]) -> Result<()> {
        let mut meter = self.spool_writes.lock().unwrap();
        let end = file
            .stream_position()?
            .checked_add(bytes.len() as u64)
            .context("write range overflow")?;
        self.admit_spool_growth(&mut meter, file, end)?;
        let written = (|| -> Result<()> {
            if super::crash::armed("spool.partial_write") {
                file.write_all(&bytes[..bytes.len() / 2])?;
                super::crash::point("spool.partial_write")?;
            }
            file.write_all(bytes)?;
            Ok(())
        })();
        if written.is_err() {
            meter.invalidate();
        }
        written
    }
    /// Resize an unsealed spool file under the same budget as writes.
    pub(crate) fn resize_spool(&self, file: &File, len: u64) -> Result<()> {
        let mut meter = self.spool_writes.lock().unwrap();
        self.admit_spool_growth(&mut meter, file, len)?;
        // A shrink only makes the meter overstate, which admission rechecks.
        let resized = file.set_len(len);
        if resized.is_err() {
            meter.invalidate();
        }
        Ok(resized?)
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
        let removed = fs::remove_dir_all(&dir);
        self.spool_writes.lock().unwrap().invalidate();
        match removed {
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
        let fp = crate::manifest::manifest_fingerprint(&manifest).unwrap();
        d.commit_uploaded(
            i,
            Some(Content {
                hash: i.hash.clone(),
                size: i.size,
                manifest,
                pack: None,
            }),
        )
        .unwrap();
        fp
    }
    #[test]
    fn cleanup_waits_for_readers_publication_and_preserves_unknown_writes() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.pool_sync_roots = vec!["crypt:shared".into()];
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
    fn linked_local_move_adds_no_spool_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.spool_limit = 5;
        let i = write(&d, b"four");
        d.rename_file("file", "moved").unwrap();
        assert_eq!(d.spool_bytes().unwrap(), 4);
        assert_eq!(fs::read(d.spool_path(&i)).unwrap(), b"four");
        assert!(d.view().unwrap().contains_key("moved"));
    }
    #[test]
    fn copied_local_move_obeys_spool_limit() {
        use super::super::virtual_drive::move_hooks::NO_LINK;
        let temp = tempfile::tempdir().unwrap();
        let mut d = fixture(temp.path());
        d.spool_limit = 5;
        let i = write(&d, b"four");
        NO_LINK.with(|n| n.set(true));
        let moved = d.rename_file("file", "moved");
        NO_LINK.with(|n| n.set(false));
        assert!(moved.is_err());
        assert_eq!(fs::read(d.spool_path(&i)).unwrap(), b"four");
        assert!(d.view().unwrap().contains_key("file"));
        assert!(!d.view().unwrap().contains_key("moved"));
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
    #[test]
    fn spool_meter_tracks_writes_resizes_and_discards_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let known = |d: &VirtualDrive| d.spool_writes.lock().unwrap().known();
        let a = d.begin("a").unwrap();
        let mut fa = File::create(d.spool_path(&a)).unwrap();
        d.write_spool_bytes(&mut fa, b"hello").unwrap();
        let b = d.begin("b").unwrap();
        let mut fb = File::create(d.spool_path(&b)).unwrap();
        d.write_spool_bytes(&mut fb, b"wide world").unwrap();
        fa.seek(SeekFrom::Start(2)).unwrap();
        d.write_spool_bytes(&mut fa, b"LLOOO").unwrap();
        d.resize_spool(&fb, 20).unwrap();
        assert_eq!(known(&d), Some(d.spool_bytes().unwrap()));
        assert_eq!(known(&d), Some(27));
        d.discard_unsealed(&b).unwrap();
        assert_eq!(known(&d), None);
        d.write_spool_bytes(&mut fa, b"!").unwrap();
        assert_eq!(known(&d), Some(d.spool_bytes().unwrap()));
        assert_eq!(known(&d), Some(8));
    }
}
