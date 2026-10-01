//! Streaming test data: never held in memory or staged on disk.
//!
//! File `i` of a run is the BLAKE3 XOF output of `keyed(seed, i)`: an
//! incompressible, deterministic stream that any reader can regenerate. The
//! uploader hashes exactly what it hands to the writer; the downloader hashes
//! what comes back, so `verified` compares the two digests.
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Random per-run key for the data stream.
pub(crate) fn random_seed() -> Result<[u8; 32]> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| anyhow!("random test seed: {e}"))?;
    Ok(seed)
}

fn cancelled() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "speed test cancelled")
}

/// `Read` of `len` pseudo-random bytes of file `index`, hashed as it is read.
pub(crate) struct TestSource<'a> {
    xof: blake3::OutputReader,
    remaining: u64,
    hasher: Hasher,
    moved: &'a AtomicU64,
    cancel: &'a AtomicBool,
}

impl<'a> TestSource<'a> {
    pub(crate) fn new(
        seed: &[u8; 32],
        index: u64,
        len: u64,
        moved: &'a AtomicU64,
        cancel: &'a AtomicBool,
    ) -> Self {
        let xof = Hasher::new_keyed(seed)
            .update(&index.to_le_bytes())
            .finalize_xof();
        Self {
            xof,
            remaining: len,
            hasher: Hasher::new(),
            moved,
            cancel,
        }
    }
    /// Digest of the bytes handed out so far (all of them once exhausted).
    pub(crate) fn digest(&self) -> blake3::Hash {
        self.hasher.finalize()
    }
    pub(crate) fn remaining(&self) -> u64 {
        self.remaining
    }
}

impl Read for TestSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let n = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        let out = &mut buf[..n];
        self.xof.fill(out);
        self.hasher.update(out);
        self.remaining -= n as u64;
        self.moved.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Hashes read-back bytes, refusing more than `limit`; stops on cancel.
pub(crate) struct VerifySink<'a> {
    hasher: Hasher,
    count: u64,
    limit: u64,
    moved: &'a AtomicU64,
    cancel: &'a AtomicBool,
}

impl<'a> VerifySink<'a> {
    pub(crate) fn new(limit: u64, moved: &'a AtomicU64, cancel: &'a AtomicBool) -> Self {
        Self {
            hasher: Hasher::new(),
            count: 0,
            limit,
            moved,
            cancel,
        }
    }
    /// Whether exactly `limit` bytes arrived and they hash to `expected`.
    pub(crate) fn matches(&self, expected: &blake3::Hash) -> bool {
        self.count == self.limit && self.hasher.finalize() == *expected
    }
    pub(crate) fn count(&self) -> u64 {
        self.count
    }
}

impl Write for VerifySink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if bytes.len() as u64 > self.limit - self.count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "read back more bytes than were written",
            ));
        }
        self.hasher.update(bytes);
        self.count += bytes.len() as u64;
        self.moved.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_in_chunks(seed: &[u8; 32], index: u64, len: u64, chunk: usize) -> (Vec<u8>, String) {
        let (moved, cancel) = (AtomicU64::new(0), AtomicBool::new(false));
        let mut source = TestSource::new(seed, index, len, &moved, &cancel);
        let mut out = Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            let n = source.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(source.remaining(), 0);
        assert_eq!(moved.load(Ordering::Relaxed), len);
        (out, source.digest().to_hex().to_string())
    }

    #[test]
    fn same_seed_gives_same_bytes_independent_of_chunking() {
        let seed = [7u8; 32];
        let len = 3 * 65536 + 17;
        let (a, ha) = read_in_chunks(&seed, 2, len, 65536);
        let (b, hb) = read_in_chunks(&seed, 2, len, 1000);
        let (c, hc) = read_in_chunks(&seed, 2, len, 1);
        assert_eq!(a.len() as u64, len);
        assert!(a == b && b == c);
        assert!(ha == hb && hb == hc);
        assert_eq!(ha, blake3::hash(&a).to_hex().to_string());
        // Other file index or seed: other bytes.
        assert_ne!(read_in_chunks(&seed, 3, len, 4096).0, a);
        assert_ne!(read_in_chunks(&[8u8; 32], 2, len, 4096).0, a);
        // Not trivially compressible: many distinct byte values.
        let distinct: BTreeSet<u8> = a.iter().copied().collect();
        assert!(distinct.len() > 250);
    }

    #[test]
    fn sink_verifies_length_and_content() {
        let seed = random_seed().unwrap();
        let (data, _) = read_in_chunks(&seed, 0, 10_000, 4096);
        let expected = blake3::hash(&data);
        let (moved, cancel) = (AtomicU64::new(0), AtomicBool::new(false));
        let mut sink = VerifySink::new(10_000, &moved, &cancel);
        sink.write_all(&data[..5000]).unwrap();
        assert!(!sink.matches(&expected), "short");
        sink.write_all(&data[5000..]).unwrap();
        assert!(sink.matches(&expected));
        assert!(sink.write_all(b"x").is_err(), "longer than written");
        let mut wrong = VerifySink::new(10_000, &moved, &cancel);
        let mut corrupted = data.clone();
        corrupted[9] ^= 1;
        wrong.write_all(&corrupted).unwrap();
        assert!(!wrong.matches(&expected));
    }

    #[test]
    fn cancel_stops_source_and_sink() {
        let (moved, cancel) = (AtomicU64::new(0), AtomicBool::new(false));
        let mut source = TestSource::new(&[1; 32], 0, 1 << 20, &moved, &cancel);
        let mut buf = [0u8; 1024];
        assert_eq!(source.read(&mut buf).unwrap(), 1024);
        cancel.store(true, Ordering::Release);
        assert!(source.read(&mut buf).is_err());
        let mut sink = VerifySink::new(10, &moved, &cancel);
        assert!(sink.write(b"abc").is_err());
    }
}
