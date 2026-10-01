//! Process-wide bandwidth pacing of the bytes RPool itself pipes to and from
//! rclone (subprocess stdin/stdout and daemon reads). rclone's own
//! `--bwlimit` applies per rclone process, so with many parallel subprocesses
//! it would multiply; these buckets cap the sum of this RPool process.
//!
//! One token bucket per key (global upload, global download, and per account
//! and direction when the account has its own limit). A bucket may go into
//! debt by one chunk; the caller then waits until the debt is repaid, which
//! keeps the long-run rate exact without splitting chunks.
use super::process;
use crate::storage::traits::OperationContext;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// At most this much unused allowance builds up while idle.
const BURST: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct State {
    /// Allowance in bytes; negative = debt.
    tokens: f64,
    last: Instant,
    rate: u64,
}

#[derive(Debug)]
pub(crate) struct Bucket {
    state: Mutex<State>,
}

impl Bucket {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                tokens: 0.0,
                last: Instant::now(),
                rate: 0,
            }),
        }
    }
    /// Takes `bytes` at `rate` bytes/s from `now`; returns how long the
    /// caller must wait. A rate change restarts the allowance.
    fn take_at(&self, bytes: u64, rate: u64, now: Instant) -> Duration {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.rate != rate {
            *state = State {
                tokens: 0.0,
                last: now,
                rate,
            };
        }
        let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
        let burst = rate as f64 * BURST.as_secs_f64();
        state.tokens = (state.tokens + elapsed * rate as f64).min(burst);
        state.last = now;
        state.tokens -= bytes as f64;
        if state.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-state.tokens / rate as f64)
        }
    }
    /// Waits for `bytes` at `rate` (`None` = unlimited). Stops waiting early
    /// on cancellation or deadline; the caller's own checks report those.
    pub(crate) fn take(&self, ctx: &OperationContext, bytes: u64, rate: Option<u64>) {
        let Some(rate) = rate.filter(|r| *r > 0) else {
            return;
        };
        let wait = self.take_at(bytes, rate, Instant::now());
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if process::check(ctx).is_err() {
                return;
            }
            std::thread::sleep((until - Instant::now()).min(process::POLL * 5));
        }
    }
}

/// What a bucket paces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Global { upload: bool },
    Account { name: String, upload: bool },
}

/// The bucket of `key`, created on first use.
pub(crate) fn bucket(key: &Key) -> Arc<Bucket> {
    static TABLE: OnceLock<Mutex<HashMap<Key, Arc<Bucket>>>> = OnceLock::new();
    TABLE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(key.clone())
        .or_insert_with(|| Arc::new(Bucket::new()))
        .clone()
}

/// Current rate of `key` from the account settings (`None` = unlimited).
pub(crate) fn current_rate(key: &Key) -> Option<u64> {
    let settings = crate::storage::account::runtime::settings();
    let now = super::traffic::now_unix();
    let offset = crate::storage::account::runtime::offset();
    let (rate, upload) = match key {
        Key::Global { upload } => (settings.global_rate(now, offset).0, *upload),
        Key::Account { name, upload } => (settings.account_rate(name, now, offset), *upload),
    };
    if upload {
        rate.up
    } else {
        rate.down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debt_model_keeps_the_rate_and_caps_idle_burst() {
        let bucket = Bucket::new();
        let t0 = Instant::now();
        // 1000 B/s: a first 500 B chunk waits 0.5 s, the next one 1 s total.
        assert_eq!(bucket.take_at(500, 1000, t0), Duration::from_millis(500));
        assert_eq!(bucket.take_at(500, 1000, t0), Duration::from_millis(1000));
        // After repaying (1 s later) the debt is gone.
        assert_eq!(
            bucket.take_at(0, 1000, t0 + Duration::from_secs(1)),
            Duration::ZERO
        );
        // A long idle period saves at most BURST worth of allowance.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(bucket.take_at(500, 1000, later), Duration::ZERO);
        assert_eq!(bucket.take_at(500, 1000, later), Duration::from_millis(500));
        // A rate change restarts the allowance at the new rate.
        assert_eq!(bucket.take_at(100, 100, later), Duration::from_secs(1));
    }

    #[test]
    fn unlimited_and_cancelled_never_wait() {
        let bucket = bucket(&Key::Account {
            name: "pacer-test".into(),
            upload: true,
        });
        let started = Instant::now();
        bucket.take(&OperationContext::none(), 1 << 30, None);
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        bucket.take(&OperationContext::with_cancel(flag), 1 << 30, Some(1));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(current_rate(&Key::Global { upload: true }), None);
    }
}
