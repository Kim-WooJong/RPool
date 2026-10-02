//! One unsealed write generation of a file: a begun intent and its spool image.
//! All write handles of the file share it. Its bytes are not acknowledged
//! until `seal` succeeds.
//!
//! Sealing a large image is slow (fsync, then a full BLAKE3 pass), so the core
//! hashes with no lock held and caches the result here, keyed by `version`;
//! `seal` then only records the intent. Any write or truncate bumps `version`,
//! which makes a cached hash stale, and `seal` falls back to hashing itself.
//!
//! Starting a generation copies (or clones, see `clone`) the kept baseline,
//! which can be the whole file. The core runs `start` holding only the file's
//! slot start lock (see `slot`), never the namespace or generation locks.
use crate::mount::namespace::Intent;
use crate::mount::native_ancestry::Ancestry;
use crate::mount::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;

const COPY_CHUNK: usize = 1024 * 1024;

pub(super) struct Generation {
    pub(super) intent: Intent,
    file: File,
    pub(super) size: u64,
    /// Bumped by every change to the image, all made under the slot lock.
    pub(super) version: u64,
    /// `(version, size, hash)` of a flushed, hashed image (`FsCore::prehash`).
    hashed: Option<(u64, u64, String)>,
    _lease: Arc<()>,
}
impl Generation {
    /// Begin from `visible`, keeping a copy-on-write baseline of its first
    /// `keep` bytes. A failed start leaves no spool behind.
    pub(super) fn start(
        drive: &VirtualDrive,
        path: &str,
        visible: Option<&Revision>,
        keep: u64,
        ancestry: Option<&Ancestry>,
    ) -> Result<Self> {
        if let Some(ancestry) = ancestry {
            let intent = drive.begin_based(path, visible, ancestry)?;
            return Self::open_spool(drive, intent, visible, keep);
        }
        let mut intent = drive.begin_observed(path, visible)?;
        // The latest pending intent at this path (a seal or deletion made
        // earlier in this workspace) is what the caller sees, so this
        // generation is its trusted continuation, not a sibling edit. A cloud
        // revision keeps the drive's observed ancestry.
        if !matches!(visible, Some(Revision::Cloud { .. })) {
            let state = drive
                .state
                .lock()
                .map_err(|_| anyhow!("namespace lock poisoned"))?;
            if let Some(previous) = state.pending.iter().rev().find(|i| i.path == path) {
                intent.depends_on = Some(previous.id.clone());
                intent.event_path = previous.event_path.clone();
                intent.parents.clear();
            }
        }
        Self::open_spool(drive, intent, visible, keep)
    }
    fn open_spool(
        drive: &VirtualDrive,
        intent: Intent,
        visible: Option<&Revision>,
        keep: u64,
    ) -> Result<Self> {
        let end = visible.map_or(0, |revision| keep.min(revision.size()));
        let target = drive.spool_path(&intent);
        #[cfg(test)]
        if end > 0 {
            baseline_hook::run(&intent.path);
        }
        // A local sealed image is cloned copy-on-write where the filesystem
        // can; anything else (and a failed clone) is copied below.
        let cloned = match visible {
            Some(Revision::Local { path, size, .. }) if end > 0 => {
                super::clone::clone_image(drive, path, &target, *size)
            }
            _ => Ok(None),
        };
        let file = match cloned {
            Ok(Some(file)) => Ok((file, true)),
            Ok(None) => OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&target)
                .map(|file| (file, false))
                .map_err(Into::into),
            Err(error) => Err(error),
        };
        let (file, cloned) = match file {
            Ok(opened) => opened,
            Err(error) => {
                let _ = drive.discard_unsealed(&intent);
                return Err(error);
            }
        };
        let mut generation = Self {
            _lease: drive.local_lease(&intent.id),
            intent,
            file,
            size: 0,
            version: 0,
            hashed: None,
        };
        let baseline = match visible {
            Some(_) if cloned => generation.trim_clone(drive, end),
            Some(revision) => generation.copy_baseline(drive, revision, end),
            None => Ok(()),
        };
        if let Err(error) = baseline {
            let _ = generation.discard(drive);
            return Err(error);
        }
        Ok(generation)
    }
    /// A clone holds the whole revision; keep only its first `end` bytes.
    fn trim_clone(&mut self, drive: &VirtualDrive, end: u64) -> Result<()> {
        self.version += 1;
        self.size = self.file.metadata()?.len();
        if end < self.size {
            self.truncate(drive, end)?;
        }
        Ok(())
    }
    fn copy_baseline(&mut self, drive: &VirtualDrive, revision: &Revision, end: u64) -> Result<()> {
        let mut offset = 0;
        while offset < end {
            let count = ((end - offset) as usize).min(COPY_CHUNK);
            let bytes = drive.read(revision, offset, count)?;
            if bytes.is_empty() {
                bail!("baseline revision ended early");
            }
            self.write_at(drive, offset, &bytes)?;
            offset += bytes.len() as u64;
        }
        Ok(())
    }
    pub(super) fn write_at(
        &mut self,
        drive: &VirtualDrive,
        offset: u64,
        bytes: &[u8],
    ) -> Result<()> {
        self.version += 1;
        self.file.seek(SeekFrom::Start(offset))?;
        let result = drive.write_spool_bytes(&mut self.file, bytes);
        self.size = self.file.metadata()?.len();
        result
    }
    pub(super) fn truncate(&mut self, drive: &VirtualDrive, len: u64) -> Result<()> {
        self.version += 1;
        drive.resize_spool(&self.file, len)?;
        self.size = len;
        Ok(())
    }
    pub(super) fn read_at(&mut self, offset: u64, count: usize) -> Result<Vec<u8>> {
        if offset >= self.size {
            return Ok(vec![]);
        }
        let mut bytes = vec![0; (count as u64).min(self.size - offset) as usize];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    /// Whether a hash of the current image is cached.
    pub(super) fn hashed(&self) -> bool {
        self.hashed.as_ref().is_some_and(|h| h.0 == self.version)
    }
    /// Cache a `hash_spool` result taken while the image was at `version`.
    pub(super) fn set_hashed(&mut self, version: u64, size: u64, hash: String) {
        if version == self.version {
            self.hashed = Some((version, size, hash));
        }
    }
    /// Durable local acknowledgement: fsync the image, record and publish the
    /// intent as pending. On error the generation stays unsealed. A cached
    /// hash of the unchanged image (already fsynced) skips the flush and hash.
    pub(super) fn seal(&self, drive: &VirtualDrive) -> Result<()> {
        match &self.hashed {
            Some((version, size, hash)) if *version == self.version => {
                let mut intent = self.intent.clone();
                intent.size = *size;
                intent.hash = hash.clone();
                drive.seal_hashed(intent)
            }
            _ => {
                self.file.sync_all().context("seal: flush spool image")?;
                drive.seal(self.intent.clone())
            }
        }
    }
    /// Drop never-acknowledged bytes.
    pub(super) fn discard(self, drive: &VirtualDrive) -> Result<()> {
        let Self { intent, file, .. } = self;
        drop(file);
        drive.discard_unsealed(&intent)
    }
}

/// Test hook run on the starting thread just before a generation copies (or
/// clones) its baseline, with exactly the locks a baseline copy holds.
#[cfg(test)]
pub(super) mod baseline_hook {
    use std::cell::RefCell;
    type Hook = Box<dyn FnMut(&str)>;
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = RefCell::new(None);
    }
    /// Install `hook` (given the file's path) for starts on this thread.
    pub(crate) fn set(hook: impl FnMut(&str) + 'static) {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }
    pub(crate) fn clear() {
        HOOK.with(|h| *h.borrow_mut() = None);
    }
    pub(super) fn run(path: &str) {
        HOOK.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook(path);
            }
        });
    }
}
