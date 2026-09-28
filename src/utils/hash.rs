use crate::prelude::*;

pub(crate) fn hash_file_range(path: &Path, offset: u64, size: u64) -> Result<String> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = size;
    let mut buffer = vec![0u8; IO_BUFFER];
    let mut hasher = Hasher::new();

    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let n = file.read(&mut buffer[..want])?;
        if n == 0 {
            bail!("unexpected EOF while hashing file range");
        }
        hasher.update(&buffer[..n]);
        remaining -= n as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}
