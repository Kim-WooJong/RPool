//! Retry bookkeeping of failed pending uploads: exponential backoff per
//! intent and one log line per distinct error (not one per attempt).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// First retry delay; doubles per failure up to [`RETRY_MAX`].
pub(crate) const RETRY_FIRST: Duration = Duration::from_secs(5);
/// Longest retry delay (the doubling stops here).
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(120);

/// Failure history of one pending intent.
struct Entry {
    /// Consecutive failures.
    failures: u32,
    /// When the next attempt may start.
    due: Instant,
    /// Last error text, to log only changed errors.
    error: String,
}

/// Backoff state of failed uploads by intent id, owned by the uploader
/// thread across upload passes.
#[derive(Default)]
pub(crate) struct RetryBook {
    /// Failure history by intent id.
    entries: BTreeMap<String, Entry>,
}
impl RetryBook {
    /// Backoff after `failures` failures: 5 s doubled per failure, capped at
    /// [`RETRY_MAX`].
    pub(crate) fn delay(failures: u32) -> Duration {
        let shift = failures.saturating_sub(1).min(16);
        RETRY_FIRST.saturating_mul(1u32 << shift).min(RETRY_MAX)
    }
    /// Whether `key` may run at `now` (never failed, or its backoff elapsed).
    pub(crate) fn due(&self, key: &str, now: Instant) -> bool {
        self.entries.get(key).is_none_or(|e| e.due <= now)
    }
    /// Records a failure. Returns the backoff when this error text differs
    /// from the previous one of `key` (the caller logs it then), else `None`.
    pub(crate) fn fail(&mut self, key: &str, error: String, now: Instant) -> Option<Duration> {
        let entry = self.entries.entry(key.to_owned()).or_insert(Entry {
            failures: 0,
            due: now,
            error: String::new(),
        });
        entry.failures = entry.failures.saturating_add(1);
        let delay = Self::delay(entry.failures);
        entry.due = now.checked_add(delay).unwrap_or(now);
        let changed = entry.error != error;
        entry.error = error;
        changed.then_some(delay)
    }
    /// Success: forget the failure history. True when it had failed before.
    pub(crate) fn succeed(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }
    /// Earliest backoff still running at `now`.
    pub(crate) fn next_due(&self, now: Instant) -> Option<Instant> {
        self.entries
            .values()
            .map(|e| e.due)
            .filter(|d| *d > now)
            .min()
    }
    /// Drops entries whose intent no longer exists (committed elsewhere).
    pub(crate) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|key, _| keep(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_cap_and_errors_log_once_per_change() {
        let delays: Vec<_> = (1..=7).map(RetryBook::delay).collect();
        assert_eq!(
            delays.iter().map(Duration::as_secs).collect::<Vec<_>>(),
            [5, 10, 20, 40, 80, 120, 120]
        );
        let mut book = RetryBook::default();
        let now = Instant::now();
        assert!(book.due("a", now));
        assert_eq!(book.fail("a", "boom".into(), now), Some(RETRY_FIRST));
        assert!(!book.due("a", now));
        assert!(book.due("a", now + RETRY_FIRST));
        assert_eq!(book.next_due(now), Some(now + RETRY_FIRST));
        assert_eq!(
            book.fail("a", "boom".into(), now),
            None,
            "same error: quiet"
        );
        assert!(book.fail("a", "other".into(), now).is_some());
        assert!(book.succeed("a"));
        assert!(!book.succeed("a"));
        assert!(book.due("a", now));
    }
}
