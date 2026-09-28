//! Owned portable upload snapshot: all data and parity use one byte sequence.
use crate::prelude::*;
use crate::utils::hash_file_range;
pub(crate) struct UploadSource {
    owner: tempfile::TempDir,
    size: u64,
}
impl UploadSource {
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
        Ok(Self { owner, size })
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.owner.path().join("upload-source")
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}
