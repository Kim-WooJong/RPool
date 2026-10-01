//! Process-wide traffic counters per rclone remote name (the part before `:`
//! of an address, as for the concurrency caps in `limit`).
//!
//! Every cloud byte of this process goes through `process::run` subprocesses
//! or the shared read daemon; both count here, once per transferred chunk
//! (atomics only, no lock per chunk). An operation is counted once, when it
//! finishes with a definite outcome; an operation that falls back to another
//! route (daemon -> subprocess) is counted by the route that answered.
//!
//! Native crypt writes address the crypt's *base* remote; their context
//! carries an attribution so they count for the crypt remote of the pool.
use crate::storage::error::{StorageError, StorageErrorKind};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

const RING: u64 = 16;
const BYTE_BITS: u32 = 40;
const BYTE_MASK: u64 = (1 << BYTE_BITS) - 1;
const TAG_MASK: u64 = (1 << (64 - BYTE_BITS)) - 1;
/// Longest kept last-error text.
const ERROR_TEXT: usize = 200;

/// Bytes per wall-clock second for the last `RING` seconds. Each slot packs a
/// 24-bit second tag and a 40-bit byte count into one atomic word.
#[derive(Default)]
pub(crate) struct Ring {
    slots: [AtomicU64; RING as usize],
}
impl Ring {
    pub(crate) fn add(&self, second: u64, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let slot = &self.slots[(second % RING) as usize];
        let tag = second & TAG_MASK;
        let mut current = slot.load(Relaxed);
        loop {
            let base = if current >> BYTE_BITS == tag {
                current & BYTE_MASK
            } else {
                0
            };
            let next = (tag << BYTE_BITS) | base.saturating_add(bytes).min(BYTE_MASK);
            match slot.compare_exchange_weak(current, next, Relaxed, Relaxed) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
    fn bytes_in(&self, second: u64) -> u64 {
        let value = self.slots[(second % RING) as usize].load(Relaxed);
        if value >> BYTE_BITS == second & TAG_MASK {
            value & BYTE_MASK
        } else {
            0
        }
    }
    /// Average bytes per second over the `window` complete seconds before
    /// `now` (the current, partial second is left out).
    pub(crate) fn rate(&self, now: u64, window: u64) -> f64 {
        let window = window.clamp(1, RING - 1);
        let total: u64 = (1..=window)
            .filter_map(|ago| now.checked_sub(ago))
            .map(|second| self.bytes_in(second))
            .sum();
        total as f64 / window as f64
    }
}

/// Live counters of one remote name.
#[derive(Default)]
pub(crate) struct Counters {
    sent: AtomicU64,
    acked: AtomicU64,
    verified: AtomicU64,
    received: AtomicU64,
    active_uploads: AtomicU32,
    active_downloads: AtomicU32,
    ok_ops: AtomicU64,
    failed_ops: AtomicU64,
    retries: AtomicU64,
    /// Unix seconds, 0 = never.
    last_ok: AtomicU64,
    last_failed: AtomicU64,
    /// First failure since the last success, 0 = none.
    failing_since: AtomicU64,
    /// Only written on failures, which are rare.
    last_error: Mutex<Option<(String, u64)>>,
    upload: Ring,
    download: Ring,
}

/// Point-in-time copy of one remote's counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Traffic {
    /// Bytes fed to rclone for uploads (stdin of `rcat`).
    pub sent_bytes: u64,
    /// Bytes of uploads rclone acknowledged (exited 0).
    pub acked_bytes: u64,
    /// Bytes read back and verified after an upload (credited by the writer).
    pub verified_bytes: u64,
    /// Bytes received from rclone: data reads and metadata listings.
    pub received_bytes: u64,
    pub active_uploads: u32,
    pub active_downloads: u32,
    pub ok_ops: u64,
    pub failed_ops: u64,
    /// Provider-rejected writes retried with backoff.
    pub retries: u64,
    pub last_ok_unix: Option<u64>,
    pub last_failed_unix: Option<u64>,
    /// First failure after the last success (no success since).
    pub failing_since_unix: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_unix: Option<u64>,
    pub upload_rate_1s: f64,
    pub upload_rate_10s: f64,
    pub download_rate_1s: f64,
    pub download_rate_10s: f64,
}

fn nonzero(value: &AtomicU64) -> Option<u64> {
    Some(value.load(Relaxed)).filter(|v| *v != 0)
}

impl Counters {
    fn snapshot(&self, now: u64) -> Traffic {
        let error = self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Traffic {
            sent_bytes: self.sent.load(Relaxed),
            acked_bytes: self.acked.load(Relaxed),
            verified_bytes: self.verified.load(Relaxed),
            received_bytes: self.received.load(Relaxed),
            active_uploads: self.active_uploads.load(Relaxed),
            active_downloads: self.active_downloads.load(Relaxed),
            ok_ops: self.ok_ops.load(Relaxed),
            failed_ops: self.failed_ops.load(Relaxed),
            retries: self.retries.load(Relaxed),
            last_ok_unix: nonzero(&self.last_ok),
            last_failed_unix: nonzero(&self.last_failed),
            failing_since_unix: nonzero(&self.failing_since),
            last_error_unix: error.as_ref().map(|(_, at)| *at),
            last_error: error.map(|(text, _)| text),
            upload_rate_1s: self.upload.rate(now, 1),
            upload_rate_10s: self.upload.rate(now, 10),
            download_rate_1s: self.download.rate(now, 1),
            download_rate_10s: self.download.rate(now, 10),
        }
    }
    fn succeeded(&self, now: u64) {
        self.ok_ops.fetch_add(1, Relaxed);
        self.last_ok.fetch_max(now, Relaxed);
        self.failing_since.store(0, Relaxed);
    }
    fn failed(&self, now: u64, error: &StorageError) {
        self.failed_ops.fetch_add(1, Relaxed);
        self.last_failed.fetch_max(now, Relaxed);
        let _ = self
            .failing_since
            .compare_exchange(0, now, Relaxed, Relaxed);
        let text = one_line(&error.to_string());
        *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((text, now));
    }
}

/// First line of a classified error, bounded. Never raw rclone stderr: the
/// storage errors carry only fixed, non-secret detail.
fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    match line.char_indices().nth(ERROR_TEXT) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_owned(),
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

type Table = RwLock<HashMap<String, Arc<Counters>>>;
fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

/// Counters of `remote` (a remote name), created on first use.
pub(crate) fn counters(remote: &str) -> Arc<Counters> {
    if let Some(found) = table()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(remote)
    {
        return found.clone();
    }
    table()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(remote.to_owned())
        .or_default()
        .clone()
}

/// Current traffic of `remote` (a remote name or an address); zero when this
/// process has not used it.
#[cfg(test)]
pub(crate) fn snapshot(remote: &str) -> Traffic {
    snapshot_at(remote, now_unix())
}
pub(crate) fn snapshot_at(remote: &str, now: u64) -> Traffic {
    let name = super::remote_name(remote).unwrap_or(remote);
    table()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(name)
        .map(|counters| counters.snapshot(now))
        .unwrap_or_default()
}

/// Credits `bytes` of a verified upload readback to the remote of `address`.
pub(crate) fn credit_verified(address: &str, bytes: u64) {
    if let Ok(name) = super::remote_name(address) {
        counters(name).verified.fetch_add(bytes, Relaxed);
    }
}

/// Whether an error is a provider/transport failure. Definite answers about
/// objects (missing, already there) are successful operations, and a
/// cancellation (unmount) is neither.
fn counts_as_failure(error: &StorageError) -> Option<bool> {
    match error.kind() {
        StorageErrorKind::NotFound | StorageErrorKind::AlreadyExists => Some(false),
        StorageErrorKind::Cancelled => None,
        _ => Some(true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Upload,
    Download,
    Other,
}

/// One metered rclone operation. Active while alive; counted as ok/failed
/// only by [`Op::finish`] (dropping it unfinished counts nothing).
pub(crate) struct Op {
    counters: Option<Arc<Counters>>,
    direction: Direction,
}
impl Op {
    /// `remote`: the attributed remote name (`None` = not metered).
    pub(crate) fn begin(remote: Option<&str>, direction: Direction) -> Self {
        let counters = remote.map(counters);
        if let Some(counters) = &counters {
            match direction {
                Direction::Upload => counters.active_uploads.fetch_add(1, Relaxed),
                Direction::Download => counters.active_downloads.fetch_add(1, Relaxed),
                Direction::Other => 0,
            };
        }
        Self {
            counters,
            direction,
        }
    }
    pub(crate) fn sent(&self, bytes: u64) {
        if let Some(counters) = &self.counters {
            counters.sent.fetch_add(bytes, Relaxed);
            counters.upload.add(now_unix(), bytes);
        }
    }
    pub(crate) fn received(&self, bytes: u64) {
        if let Some(counters) = &self.counters {
            counters.received.fetch_add(bytes, Relaxed);
            counters.download.add(now_unix(), bytes);
        }
    }
    pub(crate) fn acked(&self, bytes: u64) {
        if let Some(counters) = &self.counters {
            counters.acked.fetch_add(bytes, Relaxed);
        }
    }
    pub(crate) fn retry(&self) {
        if let Some(counters) = &self.counters {
            counters.retries.fetch_add(1, Relaxed);
        }
    }
    pub(crate) fn finish<T>(self, result: &Result<T, StorageError>) {
        let Some(counters) = &self.counters else {
            return;
        };
        let now = now_unix();
        match result {
            Ok(_) => counters.succeeded(now),
            Err(error) => match counts_as_failure(error) {
                Some(true) => counters.failed(now, error),
                Some(false) => counters.succeeded(now),
                None => {}
            },
        }
    }
}
impl Drop for Op {
    fn drop(&mut self) {
        if let Some(counters) = &self.counters {
            match self.direction {
                Direction::Upload => counters.active_uploads.fetch_sub(1, Relaxed),
                Direction::Download => counters.active_downloads.fetch_sub(1, Relaxed),
                Direction::Other => 0,
            };
        }
    }
}

/// A sink that counts what reached it as received bytes of `op`.
pub(crate) struct Metered<'a> {
    pub(crate) sink: &'a mut dyn std::io::Write,
    pub(crate) op: &'a Op,
}
impl std::io::Write for Metered<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.sink.write_all(bytes)?;
        self.op.received(bytes.len() as u64);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.sink.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_rates_use_complete_seconds_and_forget_old_slots() {
        let ring = Ring::default();
        ring.add(100, 1000);
        ring.add(100, 500);
        ring.add(105, 3000);
        ring.add(106, 7); // current second: excluded
        assert_eq!(ring.rate(106, 1), 3000.0);
        assert_eq!(ring.rate(106, 10), 450.0);
        assert_eq!(ring.rate(107, 1), 7.0);
        // 16 seconds later slot 100 is reused by 116 and does not leak.
        ring.add(116, 1);
        assert_eq!(ring.rate(117, 1), 1.0);
        assert_eq!(ring.rate(200, 10), 0.0);
        assert_eq!(ring.rate(0, 10), 0.0);
    }

    #[test]
    fn concurrent_counting_is_exact() {
        let remote = "traffic-test-concurrent";
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        let op = Op::begin(Some(remote), Direction::Upload);
                        op.sent(3);
                        op.received(5);
                        op.acked(2);
                        op.finish(&Ok::<(), StorageError>(()));
                    }
                });
            }
        });
        let t = snapshot(remote);
        assert_eq!(
            (t.sent_bytes, t.received_bytes, t.acked_bytes, t.ok_ops),
            (24_000, 40_000, 16_000, 8000)
        );
        assert_eq!((t.active_uploads, t.failed_ops), (0, 0));
    }

    #[test]
    fn outcomes_classify_failures_successes_and_cancellations() {
        let remote = "traffic-test-outcomes";
        let op = Op::begin(Some(remote), Direction::Download);
        assert_eq!(snapshot(remote).active_downloads, 1);
        op.finish(&Err::<(), _>(StorageError::not_found("x")));
        let t = snapshot(remote);
        assert_eq!((t.ok_ops, t.failed_ops, t.active_downloads), (1, 0, 0));
        assert!(t.last_ok_unix.is_some() && t.failing_since_unix.is_none());
        Op::begin(Some(remote), Direction::Other).finish(&Err::<(), _>(StorageError::Timeout {
            detail: "rclone timed out\nsecond line".into(),
        }));
        Op::begin(Some(remote), Direction::Other).finish(&Err::<(), _>(StorageError::Cancelled {
            detail: "x".into(),
        }));
        drop(Op::begin(Some(remote), Direction::Upload)); // unfinished
        let t = snapshot(remote);
        assert_eq!((t.ok_ops, t.failed_ops, t.active_uploads), (1, 1, 0));
        assert!(t.failing_since_unix.is_some());
        let error = t.last_error.unwrap();
        assert!(
            !error.contains('\n') && error.contains("timed out"),
            "{error}"
        );
        Op::begin(Some(remote), Direction::Other).finish(&Ok::<(), StorageError>(()));
        assert!(snapshot(remote).failing_since_unix.is_none());
        // Unmetered ops and unknown remotes are no-ops / zero.
        Op::begin(None, Direction::Upload).finish(&Ok::<(), StorageError>(()));
        assert_eq!(snapshot("traffic-test-never-used"), Traffic::default());
        credit_verified("traffic-test-outcomes:a/b", 9);
        assert_eq!(snapshot("traffic-test-outcomes:").verified_bytes, 9);
    }

    #[test]
    fn long_errors_are_bounded() {
        let text = one_line(&"x".repeat(1000));
        assert_eq!(text.chars().count(), ERROR_TEXT + 1);
    }
}
