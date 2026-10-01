//! Bounded output buffers, range sinks and counted readers for rclone I/O.

use super::*;

pub(super) struct BoundedVec {
    pub(super) bytes: Vec<u8>,
    pub(super) limit: usize,
}

#[derive(Debug)]
pub(super) struct AdminOutputCap;

impl std::fmt::Display for AdminOutputCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rclone admin output cap exceeded")
    }
}

impl std::error::Error for AdminOutputCap {}

pub(super) struct RangeSink<'a> {
    pub(super) sink: &'a mut dyn Write,
    pub(super) remaining: u64,
}

impl Write for RangeSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rclone exceeded requested range",
            ));
        }
        self.sink.write_all(bytes)?;
        self.remaining -= bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

impl Write for BoundedVec {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, AdminOutputCap));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the bytes rclone read from an upload source.
pub(super) struct Counted<'a> {
    pub(super) inner: &'a mut dyn Read,
    pub(super) bytes: u64,
}

impl Read for Counted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.bytes += n as u64;
        Ok(n)
    }
}
