//! MOVE of a sealed local spool image without reading it.
//!
//! A sealed image is immutable once acknowledged, and its size and hash were
//! recorded when it was sealed. A MOVE destination therefore hard-links the
//! image into its own spool directory and reuses that record: no byte is
//! copied or hashed, and the link adds no spool bytes (the budget scan counts
//! an inode once). Where the workspace filesystem has no hard links
//! (FAT/exFAT), the image is copied under the spool budget and hashed while it
//! is copied; the copy must reproduce the recorded hash.
//!
//! [`VirtualDrive::stage_move`] does that work before the filesystem core takes
//! its exclusive namespace lock, so a fallback copy never runs under it; the
//! MOVE then only renames the staged image into the destination's spool
//! directory. A staged image lives inside its source's spool directory
//! (`moving/<random>/content`) and holds the source's revision lease, so the
//! source directory is not reclaimed under it, and one left by a crash goes
//! with the source directory when that is reclaimed.

use super::*;

/// Images staged for one MOVE, by source revision id. Unused ones are
/// removed on drop.
#[derive(Default)]
pub(crate) struct StagedMoves {
    images: BTreeMap<String, Staged>,
}

struct Staged {
    dir: PathBuf,
    size: u64,
    hash: String,
    /// Keeps the source spool directory, which holds `dir`, from cleanup.
    source: Revision,
}

impl Drop for Staged {
    fn drop(&mut self) {
        // Best effort: what remains goes with the source spool directory.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::Cell;
    thread_local! {
        /// Simulates a workspace filesystem without hard links.
        pub(crate) static NO_LINK: Cell<bool> = const { Cell::new(false) };
        /// Full content copies made for MOVEs on this thread.
        pub(crate) static COPIES: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn copied() {
        COPIES.with(|c| c.set(c.get() + 1));
    }
}

fn link_image(source: &Path, target: &Path, size: u64) -> Result<()> {
    #[cfg(test)]
    if hooks::NO_LINK.with(|n| n.get()) {
        bail!("hard links disabled by test");
    }
    fs::hard_link(source, target)?;
    if fs::metadata(target)?.len() != size {
        let _ = fs::remove_file(target);
        bail!("linked spool image size mismatch");
    }
    Ok(())
}

impl VirtualDrive {
    /// The recorded seal of a local revision's image: its pending intent, or
    /// the intent record written at seal (a pinned, committed revision).
    fn sealed_record(&self, revision: &Revision) -> Option<(u64, String)> {
        let Revision::Local { id, path, size, .. } = revision else {
            return None;
        };
        let pending = {
            let s = self.state.lock().ok()?;
            s.pending
                .iter()
                .find(|i| &i.id == id && i.spool.as_deref() == Some(id.as_str()))
                .map(|i| (i.size, i.hash.clone()))
        };
        let record = pending.or_else(|| {
            let intent: Intent =
                crate::utils::read_json(&path.parent()?.join("intent.json")).ok()?;
            (&intent.id == id && intent.spool.as_deref() == Some(id.as_str()))
                .then_some((intent.size, intent.hash))
        });
        record.filter(|(n, hash)| n == size && !hash.is_empty())
    }
    /// Copy `source` to the new file `target` under the spool budget and
    /// return the hash of the bytes written.
    fn copy_hashing(&self, source: &Path, target: &Path) -> Result<String> {
        #[cfg(test)]
        hooks::copied();
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let mut hasher = Hasher::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            self.write_spool_bytes(&mut output, &buffer[..n])?;
        }
        output.sync_all()?;
        Ok(hasher.finalize().to_hex().to_string())
    }
    /// Place `revision`'s sealed image at the absent `target`, by hard link or
    /// else a verified copy. `None`: not a sealed local image.
    fn place_image(&self, revision: &Revision, target: &Path) -> Result<Option<(u64, String)>> {
        let Revision::Local { path, .. } = revision else {
            return Ok(None);
        };
        let Some((size, hash)) = self.sealed_record(revision) else {
            return Ok(None);
        };
        if link_image(path, target, size).is_ok() {
            return Ok(Some((size, hash)));
        }
        let copied = self.copy_hashing(path, target);
        if !copied.as_ref().is_ok_and(|h| *h == hash) {
            let _ = fs::remove_file(target);
            self.spool_writes.lock().unwrap().invalidate();
            copied?;
            bail!("sealed spool image changed during MOVE; pending data preserved");
        }
        Ok(Some((size, hash)))
    }
    /// Stage the sealed images of `from` (a file, or the files under a
    /// directory) for a MOVE, outside any filesystem lock. Paths in `skip`
    /// (with unsealed writes, sealed only under the lock) are left out.
    /// Best effort: anything not staged is moved under the lock instead.
    pub(crate) fn stage_move(&self, from: &str, skip: &BTreeSet<String>) -> StagedMoves {
        let mut moves = StagedMoves::default();
        let sources: Vec<Revision> = match self.visible_revision(from) {
            Ok(Some(revision)) => vec![revision],
            Ok(None) => {
                let prefix = format!("{from}/");
                match self.view() {
                    Ok(view) => view
                        .into_iter()
                        .filter(|(name, _)| name.starts_with(&prefix))
                        .filter(|(name, _)| !skip.contains(name))
                        .map(|(_, revision)| revision)
                        .collect(),
                    Err(_) => vec![],
                }
            }
            Err(_) => vec![],
        };
        for revision in sources {
            if skip.contains(from) || !matches!(revision, Revision::Local { .. }) {
                continue;
            }
            if let Ok(Some(staged)) = self.stage_image(revision) {
                moves.images.insert(staged.source.id().into(), staged);
            }
        }
        moves
    }
    fn stage_image(&self, revision: Revision) -> Result<Option<Staged>> {
        let Revision::Local { path, .. } = &revision else {
            return Ok(None);
        };
        let moving = path.parent().context("spool image parent")?.join("moving");
        fs::create_dir_all(&moving)?;
        let dir = moving.join(random_id()?);
        fs::create_dir(&dir)?;
        let placed = self.place_image(&revision, &dir.join("content"));
        let staged = |(size, hash)| Staged {
            dir: dir.clone(),
            size,
            hash,
            source: revision.clone(),
        };
        match placed {
            Ok(Some(record)) => Ok(Some(staged(record))),
            other => {
                let _ = fs::remove_dir_all(&dir);
                other.map(|_| None)
            }
        }
    }
    /// Give the MOVE `destination` the sealed image of `revision` and record
    /// its intent, reusing the seal's size and hash. `false`: `revision` is not
    /// a sealed local image, and the caller copies and seals it instead.
    pub(super) fn move_image(
        &self,
        revision: &Revision,
        destination: &mut Intent,
        staged: &mut StagedMoves,
    ) -> Result<bool> {
        let target = self.spool_path(destination);
        let mut record = None;
        if let Some(image) = staged.images.remove(revision.id()) {
            if fs::rename(image.dir.join("content"), &target).is_ok() {
                record = Some((image.size, image.hash.clone()));
            }
        }
        if record.is_none() {
            record = self.place_image(revision, &target)?;
        }
        let Some((size, hash)) = record else {
            return Ok(false);
        };
        destination.size = size;
        destination.hash = hash;
        // Flushes the destination directory entry with the intent record.
        self.record_intent(destination)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture;
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn write(d: &VirtualDrive, path: &str, bytes: &[u8]) -> Intent {
        d.state.lock().unwrap().bases.clear();
        let i = d.begin(path).unwrap();
        fs::write(d.spool_path(&i), bytes).unwrap();
        d.seal(i).unwrap();
        d.state.lock().unwrap().pending.last().unwrap().clone()
    }
    /// Seal hashes and MOVE copies made on this thread from now on.
    fn reads() -> impl Fn() -> (usize, usize) {
        let hashes = Rc::new(Cell::new(0));
        let counter = hashes.clone();
        crate::mount::crash::hash_hook::set(move || counter.set(counter.get() + 1));
        hooks::COPIES.with(|c| c.set(0));
        move || (hashes.get(), hooks::COPIES.with(|c| c.get()))
    }
    fn intent_at(d: &VirtualDrive, path: &str) -> Intent {
        let s = d.state.lock().unwrap();
        s.pending
            .iter()
            .rev()
            .find(|i| i.path == path && i.spool.is_some())
            .unwrap()
            .clone()
    }
    fn commit(d: &VirtualDrive, intent: &Intent) {
        let content = intent.spool.as_ref().map(|_| {
            crate::mount::virtual_tests::content(&fs::read(d.spool_path(intent)).unwrap())
        });
        d.commit_uploaded(intent, content).unwrap();
    }
    fn read_all(d: &VirtualDrive, path: &str) -> Vec<u8> {
        let revision = d.view().unwrap()[path].clone();
        d.read(&revision, 0, 1 << 20).unwrap()
    }
    #[cfg(unix)]
    fn same_inode(a: &Path, b: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (fs::metadata(a).unwrap(), fs::metadata(b).unwrap());
        a.ino() == b.ino()
    }

    #[test]
    fn pending_file_moves_by_link_without_reading_it() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let source = write(&d, "file", b"payload");
        let counted = reads();
        d.rename_file("file", "moved").unwrap();
        assert_eq!(counted(), (0, 0), "no hash, no copy");
        crate::mount::crash::hash_hook::clear();
        let moved = intent_at(&d, "moved");
        assert_ne!(moved.id, source.id);
        assert_eq!(moved.spool.as_deref(), Some(moved.id.as_str()));
        assert_eq!(
            (moved.size, moved.hash.clone()),
            (source.size, source.hash.clone())
        );
        let record: Intent =
            crate::utils::read_json(&d.spool_path(&moved).parent().unwrap().join("intent.json"))
                .unwrap();
        assert_eq!(record.hash, moved.hash);
        #[cfg(unix)]
        assert!(same_inode(&d.spool_path(&moved), &d.spool_path(&source)));
        assert_eq!(read_all(&d, "moved"), b"payload");
        assert!(!d.view().unwrap().contains_key("file"));
        assert_eq!(d.spool_bytes().unwrap(), 7, "a link adds no bytes");
    }

    #[test]
    fn committing_and_reclaiming_the_source_keeps_the_destination() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let source = write(&d, "file", b"payload");
        d.rename_file("file", "moved").unwrap();
        commit(&d, &source);
        d.cleanup_committed_spool().unwrap();
        assert!(!d.spool_path(&source).exists(), "source reclaimed");
        let moved = intent_at(&d, "moved");
        assert_eq!(read_all(&d, "moved"), b"payload");
        assert_eq!(
            crate::utils::hash_file_range(&d.spool_path(&moved), 0, moved.size).unwrap(),
            moved.hash
        );
        assert_eq!(d.spool_bytes().unwrap(), 7);
        let pending = d.state.lock().unwrap().pending.clone();
        for intent in pending {
            commit(&d, &intent);
        }
        d.pins.lock().unwrap().clear();
        d.cleanup_committed_spool().unwrap();
        assert_eq!(d.spool_bytes().unwrap(), 0);
        match &d.view().unwrap()["moved"] {
            Revision::Cloud { content, .. } => {
                assert_eq!(content.hash, blake3::hash(b"payload").to_hex().to_string())
            }
            Revision::Local { .. } => panic!("the move is committed"),
        }
    }

    #[test]
    fn deleting_the_destination_leaves_the_source_image_intact() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let source = write(&d, "file", b"payload");
        d.rename_file("file", "moved").unwrap();
        d.delete("moved").unwrap();
        let pending = d.state.lock().unwrap().pending.clone();
        for intent in pending {
            commit(&d, &intent);
        }
        assert_eq!(fs::read(d.spool_path(&source)).unwrap(), b"payload");
        d.pins.lock().unwrap().clear();
        d.cleanup_committed_spool().unwrap();
        assert_eq!(d.spool_bytes().unwrap(), 0);
        assert!(d.view().unwrap().is_empty());
    }

    #[test]
    fn folder_of_pending_files_moves_without_reading_them() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        write(&d, "dir/a", b"alpha");
        write(&d, "dir/sub/b", b"beta");
        let counted = reads();
        d.rename_directory("dir", "new").unwrap();
        assert_eq!(counted(), (0, 0));
        crate::mount::crash::hash_hook::clear();
        assert_eq!(read_all(&d, "new/a"), b"alpha");
        assert_eq!(read_all(&d, "new/sub/b"), b"beta");
        assert_eq!(
            intent_at(&d, "new/a").hash,
            blake3::hash(b"alpha").to_hex().to_string()
        );
        assert_eq!(d.spool_bytes().unwrap(), 9);
    }

    #[test]
    fn without_hard_links_the_move_copies_once_and_verifies_the_seal() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        write(&d, "file", b"payload");
        hooks::NO_LINK.with(|n| n.set(true));
        let counted = reads();
        d.rename_file("file", "moved").unwrap();
        // Hashed while copying: no second read of the copy.
        assert_eq!(counted(), (0, 1));
        crate::mount::crash::hash_hook::clear();
        hooks::NO_LINK.with(|n| n.set(false));
        let moved = intent_at(&d, "moved");
        assert_eq!(moved.hash, blake3::hash(b"payload").to_hex().to_string());
        assert_eq!(read_all(&d, "moved"), b"payload");
        assert_eq!(d.spool_bytes().unwrap(), 14, "a copy counts its bytes");
    }

    #[test]
    fn a_corrupt_source_is_not_moved_by_copy() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let source = write(&d, "file", b"payload");
        fs::write(d.spool_path(&source), b"PAYLOAD").unwrap();
        hooks::NO_LINK.with(|n| n.set(true));
        let moved = d.rename_file("file", "moved");
        hooks::NO_LINK.with(|n| n.set(false));
        assert!(moved.is_err());
        assert!(d.view().unwrap().contains_key("file"));
        assert!(!d.view().unwrap().contains_key("moved"));
    }

    #[test]
    fn staged_images_are_used_once_and_unused_ones_removed() {
        let temp = tempfile::tempdir().unwrap();
        let d = fixture(temp.path());
        let source = write(&d, "file", b"payload");
        let moving = d.spool_path(&source).parent().unwrap().join("moving");
        drop(d.stage_move("file", &BTreeSet::new()));
        assert_eq!(
            fs::read_dir(&moving).unwrap().count(),
            0,
            "unused stage removed"
        );
        let mut staged = d.stage_move("file", &BTreeSet::new());
        assert_eq!(d.spool_bytes().unwrap(), 7);
        let counted = reads();
        d.rename_file_staged("file", "moved", &mut staged).unwrap();
        assert_eq!(counted(), (0, 0));
        crate::mount::crash::hash_hook::clear();
        drop(staged);
        assert_eq!(fs::read_dir(&moving).unwrap().count(), 0);
        assert_eq!(read_all(&d, "moved"), b"payload");
        assert!(d.stage_move("other", &BTreeSet::new()).images.is_empty());
        let skip = BTreeSet::from(["moved".to_string()]);
        assert!(d.stage_move("moved", &skip).images.is_empty());
    }
}
