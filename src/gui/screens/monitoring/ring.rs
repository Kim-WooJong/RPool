//! The last minutes of live rates of one account, built from successive
//! statuses, and their scaling into a sparkline.
use std::collections::VecDeque;

/// Live graphs show this many seconds.
pub(crate) const LIVE_WINDOW_SECONDS: u64 = 600;
/// Upper bound of kept samples (statuses arrive about once per second).
const MAX_SAMPLES: usize = 1_200;

/// One live rate reading of an account, taken from a status file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sample {
    /// Status time (`NetStatus::updated_unix`), in unix seconds.
    pub unix: u64,
    /// Bytes per second (10 s average).
    pub up: f64,
    /// Download rate in bytes per second (10 s average).
    pub down: f64,
}

/// Live samples of one account over the last `LIVE_WINDOW_SECONDS`, oldest
/// first; kept per remote in `MountLive::rings` and drawn as a sparkline.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ring {
    /// Samples in time order, at most `MAX_SAMPLES`.
    samples: VecDeque<Sample>,
}

impl Ring {
    /// Adds a sample newer than the last one (older or equal ones, e.g. an
    /// unchanged status file, are ignored) and drops what left the window.
    pub(crate) fn push(&mut self, sample: Sample) {
        if self
            .samples
            .back()
            .is_some_and(|last| last.unix >= sample.unix)
        {
            return;
        }
        self.samples.push_back(sample);
        let oldest = sample.unix.saturating_sub(LIVE_WINDOW_SECONDS);
        while self
            .samples
            .front()
            .is_some_and(|first| first.unix < oldest)
            || self.samples.len() > MAX_SAMPLES
        {
            self.samples.pop_front();
        }
    }

    /// The samples, oldest first.
    pub(crate) fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    /// Number of samples kept.
    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }

    /// Highest upload or download rate in the window.
    pub(crate) fn peak(&self) -> f64 {
        self.samples
            .iter()
            .map(|s| s.up.max(s.down))
            .fold(0.0, f64::max)
    }
}

/// Points of one series in unit coordinates: x 0 (window start, `now` −
/// window) … 1 (`now`), y 0 (no traffic) … 1 (`peak`). Samples outside the
/// window are left out; a zero peak keeps every point on the baseline.
pub(crate) fn unit_points(
    ring: &Ring,
    now: u64,
    peak: f64,
    value: impl Fn(&Sample) -> f64,
) -> Vec<[f32; 2]> {
    let start = now.saturating_sub(LIVE_WINDOW_SECONDS);
    ring.samples()
        .filter(|s| s.unix >= start && s.unix <= now)
        .map(|s| {
            let x = (s.unix - start) as f32 / LIVE_WINDOW_SECONDS as f32;
            let y = if peak > 0.0 {
                (value(s) / peak).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            [x, y]
        })
        .collect()
}
