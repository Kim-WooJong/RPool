//! Keeps every running read daemon at the bandwidth of the global timetable.
//! `rclone rc core/bwlimit` takes one rate (`UP:DOWN`, no timetable), so a
//! small thread sets the rate when a daemon starts, at each timetable
//! boundary, and within `SETTINGS_POLL` after the settings change.
use super::daemon::{self, Daemon};
use crate::storage::account::bandwidth::Rate;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Longest sleep: settings changes reach the daemons this fast.
const SETTINGS_POLL: u64 = 30;

/// When the scheduler should wake next after `now`, given the next
/// timetable boundary.
pub(super) fn next_wake(now: u64, boundary: Option<u64>) -> u64 {
    let poll = now + SETTINGS_POLL;
    boundary
        .filter(|at| *at > now)
        .map_or(poll, |at| at.min(poll))
}

/// The rate every daemon should have at `now`.
fn desired(now: u64) -> (Rate, Option<u64>) {
    let settings = crate::storage::account::runtime::settings();
    settings.global_rate(now, crate::storage::account::runtime::offset())
}

/// One scheduler step at `now`: brings `daemons` to `rate` and returns when
/// to run again. Daemons already at the rate are not called.
pub(super) fn step(now: u64, rate: Rate, boundary: Option<u64>, daemons: &[Arc<Daemon>]) -> u64 {
    let value = rate.rclone_value();
    for daemon in daemons {
        daemon.apply_bwlimit(&value);
    }
    next_wake(now, boundary)
}

/// Applies the current rate to a new daemon and makes sure the scheduler
/// thread runs (once per process).
pub(super) fn on_start(new: &Arc<Daemon>) {
    let now = super::traffic::now_unix();
    let (rate, _) = desired(now);
    new.apply_bwlimit(&rate.rclone_value());
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("rpool-bwlimit".into())
        .spawn(|| loop {
            let now = super::traffic::now_unix();
            let (rate, boundary) = desired(now);
            let wake = step(now, rate, boundary, &daemon::live());
            while super::traffic::now_unix() < wake {
                if daemon::shut_down() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::account::bandwidth::Timetable;

    #[test]
    fn wakes_at_boundaries_or_to_poll_settings() {
        assert_eq!(next_wake(100, None), 130);
        assert_eq!(next_wake(100, Some(110)), 110);
        assert_eq!(next_wake(100, Some(500)), 130);
        assert_eq!(next_wake(100, Some(100)), 130);
    }

    /// A fake clock walked through a day: the scheduler wakes at each
    /// boundary and the rate it applies there is the new slot's rate.
    #[test]
    fn walking_a_day_applies_each_slot_at_its_boundary() {
        let table = Timetable::parse("08:00,512k 18:00,30M 23:00,off").unwrap();
        let day = 1_790_812_800; // Thursday 00:00 UTC
        let mut now = day;
        let mut applied: Vec<(u64, String)> = Vec::new();
        while now < day + 86_400 {
            let (rate, boundary) = table.at_unix(now, 0);
            let value = rate.rclone_value();
            if applied.last().is_none_or(|(_, last)| *last != value) {
                applied.push((now - day, value));
            }
            now = next_wake(now, boundary);
        }
        assert_eq!(
            applied,
            [
                (0, "off".to_string()),
                (8 * 3600, "524288B".to_string()),
                (18 * 3600, "31457280B".to_string()),
                (23 * 3600, "off".to_string()),
            ]
        );
        assert_eq!(step(5, Rate::OFF, None, &[]), 35);
    }
}
