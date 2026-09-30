//! One unsealed write generation of a file: a begun intent and its spool image.
//! All write handles of the file share it. Its bytes are not acknowledged
//! until `seal` succeeds.
use crate::mount::namespace::Intent;
use crate::mount::native_ancestry::Ancestry;
use crate::mount::virtual_drive::{Revision, VirtualDrive};
use crate::prelude::*;

const COPY_CHUNK: usize = 1024 * 1024;

pub(super) struct Generation {
    pub(super) intent: Intent,
    file: File,
    pub(super) size: u64,
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
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(drive.spool_path(&intent));
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                let _ = drive.discard_unsealed(&intent);
                return Err(error.into());
            }
        };
        let mut generation = Self {
            _lease: drive.local_lease(&intent.id),
            intent,
            file,
            size: 0,
        };
        if let Some(revision) = visible {
            if let Err(error) = generation.copy_baseline(drive, revision, keep) {
                let _ = generation.discard(drive);
                return Err(error);
            }
        }
        Ok(generation)
    }
    fn copy_baseline(
        &mut self,
        drive: &VirtualDrive,
        revision: &Revision,
        keep: u64,
    ) -> Result<()> {
        let end = keep.min(revision.size());
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
        self.file.seek(SeekFrom::Start(offset))?;
        let result = drive.write_spool_bytes(&mut self.file, bytes);
        self.size = self.file.metadata()?.len();
        result
    }
    pub(super) fn truncate(&mut self, drive: &VirtualDrive, len: u64) -> Result<()> {
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
    /// Durable local acknowledgement: fsync the image, record and publish the
    /// intent as pending. On error the generation stays unsealed.
    pub(super) fn seal(&self, drive: &VirtualDrive) -> Result<()> {
        self.file.sync_all()?;
        drive.seal(self.intent.clone())
    }
    /// Drop never-acknowledged bytes.
    pub(super) fn discard(self, drive: &VirtualDrive) -> Result<()> {
        let Self { intent, file, .. } = self;
        drop(file);
        drive.discard_unsealed(&intent)
    }
}
