//! Alert rules, evaluated once per sample with an explicit clock.
//!
//! - Stalled: uploads are pending but no upload byte reached rclone for
//!   `STALL_SECONDS` (measured from the later of the last upload progress
//!   and the moment the queue became non-empty).
//! - Unreachable: an account had only failures (no success in between) over
//!   at least `UNREACHABLE_SECONDS`.
//! - Errors: an account's failed operations increased in the last
//!   `ERROR_WINDOW_SECONDS` (not shown when the account is unreachable).
use super::model::{Alert, AlertKind, STALL_SECONDS};
use crate::storage::rclone::traffic::Traffic;
use std::collections::HashMap;

pub(crate) const ERROR_WINDOW_SECONDS: u64 = 60;
pub(crate) const UNREACHABLE_SECONDS: u64 = 60;

#[derive(Default)]
pub(crate) struct AlertState {
    sent_total: Option<u64>,
    upload_progress_unix: u64,
    queue_since: Option<u64>,
    errors_since: HashMap<String, u64>,
    failed_seen: HashMap<String, u64>,
}

impl AlertState {
    /// `remotes`: (pool address, its traffic); `pending_files`: queue length.
    pub(crate) fn evaluate(
        &mut self,
        now: u64,
        remotes: &[(&str, &Traffic)],
        pending_files: u64,
    ) -> Vec<Alert> {
        let mut alerts = Vec::new();
        let sent: u64 = remotes.iter().map(|(_, t)| t.sent_bytes).sum();
        if self.sent_total != Some(sent) {
            self.sent_total = Some(sent);
            self.upload_progress_unix = now;
        }
        if pending_files == 0 {
            self.queue_since = None;
        } else {
            let queued = *self.queue_since.get_or_insert(now);
            let since = queued.max(self.upload_progress_unix);
            if now.saturating_sub(since) >= STALL_SECONDS {
                alerts.push(Alert {
                    kind: AlertKind::Stalled,
                    remote: None,
                    since_unix: since,
                    message: format!(
                        "{pending_files} pending upload(s) but no upload traffic for {}s",
                        now - since
                    ),
                });
            }
        }
        for (remote, traffic) in remotes {
            let failed_before = self
                .failed_seen
                .insert((*remote).to_owned(), traffic.failed_ops);
            if let Some(since) = traffic.failing_since_unix.filter(|since| {
                traffic
                    .last_failed_unix
                    .is_some_and(|last| last.saturating_sub(*since) >= UNREACHABLE_SECONDS)
            }) {
                self.errors_since.remove(*remote);
                alerts.push(Alert {
                    kind: AlertKind::Unreachable,
                    remote: Some((*remote).to_owned()),
                    since_unix: since,
                    message: format!(
                        "{remote}: every operation failed for {}s{}",
                        now.saturating_sub(since),
                        last_error(traffic)
                    ),
                });
                continue;
            }
            // A failure observed by this monitor (not one from before it
            // started) within the window.
            let increased = failed_before.is_some_and(|before| traffic.failed_ops > before);
            let recent = traffic
                .last_failed_unix
                .is_some_and(|last| now.saturating_sub(last) < ERROR_WINDOW_SECONDS);
            if increased && recent {
                self.errors_since.entry((*remote).to_owned()).or_insert(now);
            } else if !recent {
                self.errors_since.remove(*remote);
            }
            if let Some(since) = self.errors_since.get(*remote) {
                alerts.push(Alert {
                    kind: AlertKind::Errors,
                    remote: Some((*remote).to_owned()),
                    since_unix: *since,
                    message: format!(
                        "{remote}: operations failed recently{}",
                        last_error(traffic)
                    ),
                });
            }
        }
        alerts
    }
}

fn last_error(traffic: &Traffic) -> String {
    traffic
        .last_error
        .as_deref()
        .map(|error| format!(" (last error: {error})"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(alerts: &[Alert]) -> Vec<(AlertKind, Option<&str>, u64)> {
        alerts
            .iter()
            .map(|a| (a.kind, a.remote.as_deref(), a.since_unix))
            .collect()
    }

    #[test]
    fn stalled_needs_pending_work_without_upload_progress() {
        let mut state = AlertState::default();
        let mut t = Traffic::default();
        assert!(state.evaluate(1000, &[("a:", &t)], 0).is_empty());
        assert!(state.evaluate(1001, &[("a:", &t)], 2).is_empty());
        assert!(state
            .evaluate(1001 + STALL_SECONDS - 1, &[("a:", &t)], 2)
            .is_empty());
        let alerts = state.evaluate(1001 + STALL_SECONDS, &[("a:", &t)], 2);
        assert_eq!(kinds(&alerts), [(AlertKind::Stalled, None, 1001)]);
        assert!(alerts[0].message.contains("2 pending"));
        // Upload progress resets the clock.
        t.sent_bytes = 10;
        assert!(state.evaluate(1300, &[("a:", &t)], 2).is_empty());
        assert_eq!(
            kinds(&state.evaluate(1300 + STALL_SECONDS, &[("a:", &t)], 2)),
            [(AlertKind::Stalled, None, 1300)]
        );
        // An empty queue clears it; a new queue starts a fresh wait.
        assert!(state.evaluate(1600, &[("a:", &t)], 0).is_empty());
        assert!(state.evaluate(1700, &[("a:", &t)], 1).is_empty());
    }

    #[test]
    fn recent_new_failures_raise_errors_until_the_window_passes() {
        let mut state = AlertState::default();
        let mut t = Traffic {
            failed_ops: 3,
            last_failed_unix: Some(990),
            ..Traffic::default()
        };
        // Failures from before the monitor started are not new.
        assert!(state.evaluate(1000, &[("a:", &t)], 0).is_empty());
        t.failed_ops = 4;
        t.last_failed_unix = Some(1005);
        t.last_error = Some("rclone timed out".into());
        t.last_ok_unix = Some(1004);
        let alerts = state.evaluate(1005, &[("a:", &t), ("b:", &Traffic::default())], 0);
        assert_eq!(kinds(&alerts), [(AlertKind::Errors, Some("a:"), 1005)]);
        assert!(alerts[0].message.contains("timed out"));
        assert_eq!(state.evaluate(1064, &[("a:", &t)], 0).len(), 1);
        assert!(state.evaluate(1065, &[("a:", &t)], 0).is_empty());
    }

    #[test]
    fn only_failures_for_a_minute_is_unreachable() {
        let mut state = AlertState::default();
        let mut t = Traffic {
            failed_ops: 1,
            failing_since_unix: Some(1000),
            last_failed_unix: Some(1000),
            ..Traffic::default()
        };
        state.evaluate(1000, &[("a:", &t)], 0);
        t.failed_ops = 2;
        t.last_failed_unix = Some(1030);
        assert_eq!(
            kinds(&state.evaluate(1030, &[("a:", &t)], 0)),
            [(AlertKind::Errors, Some("a:"), 1030)]
        );
        t.failed_ops = 3;
        t.last_failed_unix = Some(1060);
        assert_eq!(
            kinds(&state.evaluate(1060, &[("a:", &t)], 0)),
            [(AlertKind::Unreachable, Some("a:"), 1000)]
        );
        // A success ends the streak (the counters clear failing_since).
        t.failing_since_unix = None;
        t.last_ok_unix = Some(1070);
        assert_eq!(
            kinds(&state.evaluate(1070, &[("a:", &t)], 0)),
            Vec::<(AlertKind, Option<&str>, u64)>::new()
        );
    }
}
