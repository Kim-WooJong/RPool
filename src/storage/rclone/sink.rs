//! Bounded output buffers, range sinks and counted readers for rclone I/O.

use super::*;

/// In-memory output buffer that errors with [`AdminOutputCap`] instead of growing past `limit`.
/// Collects admin/listing output and `read_all_raw` results.
pub(super) struct BoundedVec {
    /// Bytes collected so far.
    pub(super) bytes: Vec<u8>,
    /// Maximum bytes accepted.
    pub(super) limit: usize,
}

#[derive(Debug)]
/// Marker error carried inside the `io::Error` of a [`BoundedVec`] overflow;
/// `process::sink_error` maps it to `OutputBoundsViolated`.
pub(super) struct AdminOutputCap;

impl std::fmt::Display for AdminOutputCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rclone admin output cap exceeded")
    }
}

impl std::error::Error for AdminOutputCap {}

/// Write adapter that rejects any byte beyond the requested read range, so a
/// misbehaving rclone cannot write more than asked into the caller's sink.
pub(super) struct RangeSink<'a> {
    /// The caller's destination.
    pub(super) sink: &'a mut dyn Write,
    /// Bytes still allowed; `u64::MAX` for an unbounded read.
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
    /// The wrapped upload source.
    pub(super) inner: &'a mut dyn Read,
    /// Total bytes read from it so far.
    pub(super) bytes: u64,
}

impl Read for Counted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.bytes += n as u64;
        Ok(n)
    }
}
