//! Stall detection of one streamed rclone upload (`rcat`).
//!
//! rclone uses `--retries 1 --low-level-retries 1`, and its own idle timeout
//! is minutes long, so a hung provider connection used to hold a shard (and
//! the file and upload queue behind it) for ~5 minutes. Two phases:
//! - feeding stdin: a stall is no stdin progress for [`STALL_FLOOR`]
//!   (time spent in RPool's own bandwidth throttle does not count);
//! - after stdin closed: rclone may still upload what it buffered, so it gets
//!   `max(STALL_FLOOR, bytes / STALL_RATE, 4 x feeding time)` to exit. The last
//!   term covers slow links and bandwidth limits where rclone buffers chunks.
//!
//! On expiry the process runner kills rclone and reports a retriable
//! `Timeout`; shard writes are idempotent overwrites of a verified object, so
//! the existing scheduler retry re-sends the shard from its stable spool.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) const STALL_FLOOR: Duration = Duration::from_secs(60);
/// Slowest upload rate an rclone exit after stdin closed is allowed to take.
pub(super) const STALL_RATE: u64 = 256 * 1024;

enum Phase {
    Feeding {
        started: Instant,
        progress: Instant,
    },
    Draining {
        closed: Instant,
        allowance: Duration,
    },
}

pub(super) struct Stall {
    floor: Duration,
    rate: u64,
    phase: Mutex<Phase>,
    fired: AtomicBool,
}
impl Stall {
    pub(super) fn upload() -> Self {
        Self::new(STALL_FLOOR, STALL_RATE)
    }
    pub(super) fn new(floor: Duration, rate: u64) -> Self {
        let now = Instant::now();
        Self {
            floor,
            rate: rate.max(1),
            phase: Mutex::new(Phase::Feeding {
                started: now,
                progress: now,
            }),
            fired: AtomicBool::new(false),
        }
    }
    fn phase(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|p| p.into_inner())
    }
    /// rclone accepted stdin bytes (or RPool is about to offer more).
    pub(super) fn progress(&self) {
        if let Phase::Feeding { progress, .. } = &mut *self.phase() {
            *progress = Instant::now();
        }
    }
    /// stdin closed after `total` bytes.
    pub(super) fn closed(&self, total: u64) {
        let mut phase = self.phase();
        if let Phase::Feeding { started, .. } = *phase {
            let now = Instant::now();
            let by_rate = Duration::from_secs(total / self.rate);
            let by_feed = now.saturating_duration_since(started).saturating_mul(4);
            *phase = Phase::Draining {
                closed: now,
                allowance: self.floor.max(by_rate).max(by_feed),
            };
        }
    }
    /// Checks the deadline at `now`; once expired it stays expired.
    pub(super) fn expired(&self, now: Instant) -> bool {
        if self.fired.load(Ordering::Acquire) {
            return true;
        }
        let expired = match *self.phase() {
            Phase::Feeding { progress, .. } => now.saturating_duration_since(progress) > self.floor,
            Phase::Draining { closed, allowance } => {
                now.saturating_duration_since(closed) > allowance
            }
        };
        if expired {
            self.fired.store(true, Ordering::Release);
        }
        expired
    }
    /// Whether the deadline fired (the runner then reports a retriable timeout).
    pub(super) fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeding_progress_resets_and_draining_scales_with_size() {
        let stall = Stall::new(Duration::from_millis(50), 1024);
        let start = Instant::now();
        assert!(!stall.expired(start));
        stall.progress();
        assert!(!stall.expired(Instant::now() + Duration::from_millis(10)));
        assert!(stall.expired(Instant::now() + Duration::from_millis(100)));
        assert!(stall.fired(), "expiry is sticky");

        let stall = Stall::new(Duration::from_millis(50), 1024);
        stall.closed(10 * 1024); // 10 s at 1 KiB/s
        let now = Instant::now();
        assert!(!stall.expired(now + Duration::from_secs(5)));
        assert!(stall.expired(now + Duration::from_secs(11)));
    }

    #[test]
    fn production_deadlines_match_the_documented_policy() {
        let stall = Stall::upload();
        stall.closed(2 * 1024 * 1024);
        let now = Instant::now();
        // A 2 MB shard gets the 60 s floor, not five minutes.
        assert!(!stall.expired(now + Duration::from_secs(59)));
        assert!(stall.expired(now + Duration::from_secs(61)));
        let stall = Stall::upload();
        stall.closed(64 * 1024 * 1024);
        assert!(!stall.expired(now + Duration::from_secs(250)));
    }
}
