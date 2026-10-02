//! What a speed test runs with: the rclone context, the pool's write path and
//! the stop flag.
use crate::storage::native_crypt::route::NativeCrypt;
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::OperationContext;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shared runtime of one speed test, passed to every phase.
pub(crate) struct Engine {
    /// rclone runner used for all test operations.
    pub context: RcloneContext,
    /// Set for `--native-crypt` pools: writes are encrypted by RPool and go
    /// to the crypt remote's base, exactly like `put`.
    pub native: Option<NativeCrypt>,
    /// Stop flag (Ctrl-C/SIGTERM or GUI Stop): no new work, in-flight work aborts.
    pub cancel: Arc<AtomicBool>,
    /// Set: start no further file, finish the ones in flight (a timed tuning
    /// level at its end, so every started upload also commits).
    pub drain: Arc<AtomicBool>,
}

impl Engine {
    /// Creates the engine; `native_crypt` selects RPool's own encryption path
    /// (as for `--native-crypt` pools) instead of rclone crypt writes.
    pub(crate) fn new(context: RcloneContext, native_crypt: bool, cancel: Arc<AtomicBool>) -> Self {
        let native = native_crypt.then(|| NativeCrypt::new(context.clone()));
        Self {
            context,
            native,
            cancel,
            drain: Arc::new(AtomicBool::new(false)),
        }
    }
    /// The same remotes and write path with its own stop and drain flags (a
    /// timed tuning level ends only its own transfers).
    pub(crate) fn scoped(&self, cancel: Arc<AtomicBool>, drain: Arc<AtomicBool>) -> Self {
        Self {
            context: self.context.clone(),
            native: self
                .native
                .as_ref()
                .map(|_| NativeCrypt::new(self.context.clone())),
            cancel,
            drain,
        }
    }
    /// Whether the drain flag is set (start no further file).
    pub(crate) fn draining(&self) -> bool {
        self.drain.load(Ordering::Acquire)
    }
    /// Whether the user stopped the test.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    /// Stoppable context for a timed step.
    pub(crate) fn step(&self, limit: Duration) -> OperationContext {
        OperationContext::with_deadline_and_cancel(Instant::now() + limit, self.cancel.clone())
    }
    /// Stoppable context for one transfer: one minute plus the file at
    /// 256 KiB/s, so a hung remote cannot stall the test forever.
    pub(crate) fn transfer(&self, bytes: u64) -> OperationContext {
        self.step(Duration::from_secs(60 + bytes / (256 * 1024)))
    }
    /// Cleanup context: NOT stoppable, so a stop still removes test files.
    pub(crate) fn cleanup(&self) -> OperationContext {
        OperationContext::with_deadline(Instant::now() + Duration::from_secs(120))
    }
}

/// One line, bounded, for the report (errors carry no secrets or stderr).
pub(crate) fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(200).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_are_one_short_line() {
        assert_eq!(super::one_line("a\nb"), "a");
        let long = "x".repeat(300);
        let line = super::one_line(&long);
        assert_eq!(line.chars().count(), 201);
        assert!(line.ends_with('…'));
    }
}
