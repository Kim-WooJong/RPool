//! Single-flight download of one shard. Every reader of a missing shard
//! joins the same flight; one job downloads it while the others wait on the
//! progress it publishes.
use super::index::Admission;
use crate::prelude::*;
use std::sync::Condvar;

/// How a flight ended.
#[derive(Clone)]
pub(super) enum Outcome {
    /// Verified bytes are published under the cache entry name.
    Published,
    /// Nothing was published. `retry` marks a readahead failure: a demand
    /// reader starts its own flight (with reconstruction) instead of failing.
    Failed { message: String, retry: bool },
}

struct Progress {
    /// Read handle of the partial download (an independent file object, so
    /// positional reads never move the writer's cursor).
    partial: Option<Arc<File>>,
    /// Bytes of the direct download already written to `partial`.
    written: u64,
    /// A reader was handed bytes of the unverified prefix.
    served: bool,
    outcome: Option<Outcome>,
}

pub(super) struct Flight {
    pub(super) mode: Admission,
    progress: Mutex<Progress>,
    changed: Condvar,
}

/// What a waiting reader may do next.
pub(super) enum Ready {
    Done(Outcome),
    /// The requested range is already present in the partial download.
    Prefix(Arc<File>),
}

impl Flight {
    pub(super) fn new(mode: Admission) -> Self {
        Self {
            mode,
            progress: Mutex::new(Progress {
                partial: None,
                written: 0,
                served: false,
                outcome: None,
            }),
            changed: Condvar::new(),
        }
    }
    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        // Progress holds plain counters; a panicking writer leaves them usable.
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn stream_to(&self, partial: File) {
        let mut p = self.progress();
        p.partial = Some(Arc::new(partial));
        p.written = 0;
    }
    pub(super) fn advance(&self, bytes: u64) {
        self.progress().written += bytes;
        self.changed.notify_all();
    }
    /// The direct download stopped; its bytes are no longer served. Returns
    /// whether any reader was served from its unverified prefix.
    pub(super) fn stop_stream(&self) -> bool {
        let mut p = self.progress();
        p.partial = None;
        p.written = 0;
        p.served
    }
    pub(super) fn finish(&self, outcome: Outcome) {
        let mut p = self.progress();
        p.partial = None;
        p.outcome = Some(outcome);
        drop(p);
        self.changed.notify_all();
    }
    /// Wait until the flight ends or, when `early` allows it, until the bytes
    /// `[..end)` of the direct download exist.
    pub(super) fn wait(&self, end: u64, early: bool) -> Ready {
        let mut p = self.progress();
        loop {
            if let Some(outcome) = &p.outcome {
                return Ready::Done(outcome.clone());
            }
            if early && p.written >= end {
                if let Some(file) = p.partial.as_ref().map(Arc::clone) {
                    p.served = true;
                    return Ready::Prefix(file);
                }
            }
            p = self.changed.wait(p).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Download sink: writes to the partial file and publishes progress after
/// each chunk, so waiting readers can be served from the prefix.
pub(super) struct ProgressSink<'a> {
    pub(super) file: &'a mut File,
    pub(super) flight: &'a Flight,
}
impl Write for ProgressSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write_all(bytes)?;
        self.flight.advance(bytes.len() as u64);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
