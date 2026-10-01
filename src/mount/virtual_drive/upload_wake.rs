//! Upload wake-up of a drive: every path that queues a pending intent (or
//! commits a metadata-only change) calls [`UploadControl::notify`], and the
//! mount's uploader (`mount::upload_worker`) starts within milliseconds
//! instead of waiting for the next background interval.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Files uploaded concurrently by default (`rpool mount --upload-files`).
pub(crate) const DEFAULT_UPLOAD_FILES: usize = 4;
/// Upper bound of `--upload-files`; shard transfers stay bounded by the pool's
/// `workers` whatever this is.
pub(crate) const MAX_UPLOAD_FILES: usize = 64;

pub(crate) struct UploadControl {
    /// Bumped by every notification; waiters compare against what they saw.
    generation: Mutex<u64>,
    changed: Condvar,
    files: AtomicUsize,
}
impl Default for UploadControl {
    fn default() -> Self {
        Self {
            generation: Mutex::new(0),
            changed: Condvar::new(),
            files: AtomicUsize::new(DEFAULT_UPLOAD_FILES),
        }
    }
}
impl UploadControl {
    /// Something to upload or publish was queued.
    pub(crate) fn notify(&self) {
        let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }
    pub(crate) fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|p| p.into_inner())
    }
    /// Waits until a notification after `seen`, or `timeout`. True when notified.
    pub(crate) fn wait_after(&self, seen: u64, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        while *generation == seen {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            generation = self
                .changed
                .wait_timeout(generation, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }
    /// Concurrent file uploads of one upload round (at least 1).
    pub(crate) fn files(&self) -> usize {
        self.files
            .load(Ordering::Relaxed)
            .clamp(1, MAX_UPLOAD_FILES)
    }
    pub(crate) fn set_files(&self, files: usize) {
        self.files
            .store(files.clamp(1, MAX_UPLOAD_FILES), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_notification_wakes_a_waiter_immediately() {
        let control = Arc::new(UploadControl::default());
        let seen = control.generation();
        let notifier = control.clone();
        let started = Instant::now();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            notifier.notify();
        });
        assert!(control.wait_after(seen, Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(5));
        thread.join().unwrap();
        // A notification already seen does not satisfy a later wait.
        let seen = control.generation();
        assert!(!control.wait_after(seen, Duration::from_millis(10)));
        // One that arrived before the wait does (no lost wake-up).
        control.notify();
        assert!(control.wait_after(seen, Duration::from_secs(30)));
    }

    #[test]
    fn concurrency_is_clamped() {
        let control = UploadControl::default();
        assert_eq!(control.files(), DEFAULT_UPLOAD_FILES);
        control.set_files(0);
        assert_eq!(control.files(), 1);
        control.set_files(10_000);
        assert_eq!(control.files(), MAX_UPLOAD_FILES);
    }
}
