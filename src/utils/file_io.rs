//! Positional file I/O that does not move the file cursor (`pwrite`/`pread` on
//! Unix, `seek_write`/`seek_read` on Windows), so several threads can share one
//! `File`. Used by restore, repair, reconstruction and the mount shard cache.
use crate::prelude::*;

#[cfg(unix)]
/// Writes all of `buf` at absolute `offset`, looping over short writes; a zero-byte write is `WriteZero`.
pub(crate) fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        let n = file.write_at(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write_at returned zero",
            ));
        }
        offset += n as u64;
        buf = &buf[n..];
    }
    Ok(())
}

#[cfg(windows)]
/// Writes all of `buf` at absolute `offset`, looping over short writes; a zero-byte write is `WriteZero`.
pub(crate) fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "seek_write returned zero",
            ));
        }
        offset += n as u64;
        buf = &buf[n..];
    }
    Ok(())
}

#[cfg(unix)]
/// Fills `buf` from absolute `offset`, looping over short reads; end of file first is `UnexpectedEof`.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut done = 0usize;
    while done < buf.len() {
        let n = file.read_at(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read_at returned zero",
            ));
        }
        done += n;
    }
    Ok(())
}

#[cfg(windows)]
/// Fills `buf` from absolute `offset`, looping over short reads; end of file first is `UnexpectedEof`.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < buf.len() {
        let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "seek_read returned zero",
            ));
        }
        done += n;
    }
    Ok(())
}
