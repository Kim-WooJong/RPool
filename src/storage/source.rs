//! The byte sequence an upload reads: all data and parity come from it.
//!
//! A user file (`rpool put`) may change while it uploads, so it is copied to
//! an owned snapshot first. A drive upload reads a sealed spool image, which
//! is immutable once acknowledged, so it is used in place: no copy and no
//! extra full-file hashing before the first shard leaves.
use crate::prelude::*;
use crate::utils::hash_file_range;
/// The file an upload's shards are cut from, with its fixed size. Built by
/// `commands::put` (snapshot for user files, sealed for drive spool images).
pub(crate) struct UploadSource {
    /// Owned snapshot directory (kept alive, removed on drop), or None for a
    /// sealed source used in place.
    _owner: Option<tempfile::TempDir>,
    /// File the shards are read from (snapshot copy or the sealed original).
    path: PathBuf,
    /// Byte length, checked against the file when created.
    size: u64,
}
impl UploadSource {
    /// A sealed, immutable file (drive spool image), read in place.
    pub(crate) fn sealed(source: &Path, size: u64) -> Result<Self> {
        if fs::metadata(source)?.len() != size {
            bail!("source size changed");
        }
        Ok(Self {
            _owner: None,
            path: source.to_path_buf(),
            size,
        })
    }
    /// Copies a user file into an owned temporary snapshot, verifying by BLAKE3 that
    /// neither the source nor the copy changed during the copy.
    pub(crate) fn capture(source: &Path, size: u64) -> Result<Self> {
        if fs::metadata(source)?.len() != size {
            bail!("source size changed");
        }
        let before = hash_file_range(source, 0, size)?;
        let owner = tempfile::tempdir()?;
        let path = owner.path().join("upload-source");
        let mut input = File::open(source)?;
        let mut output = File::create(&path)?;
        let count = std::io::copy(&mut (&mut input).take(size), &mut output)?;
        output.flush()?;
        drop(output);
        drop(input);
        if count != size
            || fs::metadata(source)?.len() != size
            || hash_file_range(&path, 0, size)? != before
            || hash_file_range(source, 0, size)? != before
        {
            bail!("source changed while creating upload snapshot");
        }
        Ok(Self {
            _owner: Some(owner),
            path,
            size,
        })
    }
    /// Path to read shards from.
    pub(crate) fn path(&self) -> PathBuf {
        self.path.clone()
    }
    /// Total bytes of the source.
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}
