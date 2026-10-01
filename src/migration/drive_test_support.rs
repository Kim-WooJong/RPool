//! In-memory drive for drive-migration tests: a source generation with
//! files, and new epochs whose published records are decoded by the real
//! mount code (`drive_generation_read::decode`), as a fresh PC reads them.
use super::drive_model::{DriveFile, GenerationRef, SourceView};
use super::drive_source::DriveSource;
use crate::mount::drive_generation_write::{Record, Sink};
use crate::prelude::*;

pub(crate) struct FakeDrive {
    pub source: GenerationRef,
    pub files: Mutex<Vec<DriveFile>>,
    /// Epoch -> records published there.
    pub published: Mutex<BTreeMap<String, Vec<Record>>>,
    pub fail: bool,
}

impl FakeDrive {
    pub(crate) fn new(files: Vec<DriveFile>) -> Self {
        Self {
            source: GenerationRef { epoch: None },
            files: Mutex::new(files),
            published: Mutex::new(BTreeMap::new()),
            fail: false,
        }
    }
    pub(crate) fn sink(&self, epoch: &str) -> FakeSink<'_> {
        FakeSink {
            drive: self,
            epoch: epoch.into(),
        }
    }
}

impl DriveSource for FakeDrive {
    fn generations(&self) -> Result<Vec<GenerationRef>> {
        if self.fail {
            bail!("offline");
        }
        let mut out = vec![self.source.clone()];
        for epoch in self.published.lock().unwrap().keys() {
            out.insert(
                0,
                GenerationRef {
                    epoch: Some(epoch.clone()),
                },
            );
        }
        Ok(out)
    }
    fn view(&self, generation: &GenerationRef) -> Result<SourceView> {
        if self.fail {
            bail!("offline");
        }
        if *generation == self.source {
            return Ok(SourceView {
                files: self.files.lock().unwrap().clone(),
            });
        }
        let records = generation
            .epoch
            .as_ref()
            .and_then(|e| self.published.lock().unwrap().get(e).cloned())
            .unwrap_or_default();
        crate::mount::drive_generation_read::decode(&records)
    }
}

pub(crate) struct FakeSink<'a> {
    drive: &'a FakeDrive,
    epoch: String,
}

impl Sink for FakeSink<'_> {
    fn publish(&self, record: &Record) -> Result<()> {
        let mut published = self.drive.published.lock().unwrap();
        let records = published.entry(self.epoch.clone()).or_default();
        // Write-once and content-addressed: a repeat is a no-op.
        if !records.iter().any(|r| r.id == record.id) {
            records.push(record.clone());
        }
        Ok(())
    }
}

/// A drive file over `manifest` (the path is also the plaintext).
pub(crate) fn drive_file(path: &str, revision: &str, manifest: Manifest) -> DriveFile {
    DriveFile {
        path: path.into(),
        revision: revision.into(),
        hash: blake3::hash(path.as_bytes()).to_hex().to_string(),
        size: manifest.original_size,
        manifest,
    }
}
