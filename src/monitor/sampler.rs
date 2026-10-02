//! Builds one [`NetStatus`] per sample from the rclone traffic counters and
//! the drive's queue, and the per-minute [`HistoryPoint`]s. Pure apart from
//! the `mtime` callback, so it is tested with a fake clock.
use super::alerts::AlertState;
use super::model::{HistoryPoint, NetStatus, QueueStatus, RemoteTraffic, STATUS_VERSION};
use crate::storage::rclone::traffic::Traffic;
use std::collections::HashMap;
use std::path::PathBuf;

/// One pending upload or deletion of the drive.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingItem {
    /// Stable id of the pending item (tracks its first-seen time).
    pub id: String,
    /// Bytes to upload (0 for deletions).
    pub size: u64,
    /// Spool file, whose modification time dates the item.
    pub spool: Option<PathBuf>,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
/// Cumulative counters of one remote at one sample.
struct Totals {
    /// Bytes sent to rclone (uploads).
    upload: u64,
    /// Bytes received (downloads).
    download: u64,
    /// Successful operations.
    ok: u64,
    /// Failed operations.
    failed: u64,
}
impl Totals {
    /// Counters taken from one remote's traffic snapshot.
    fn of(t: &Traffic) -> Self {
        Self {
            upload: t.sent_bytes,
            download: t.received_bytes,
            ok: t.ok_ops,
            failed: t.failed_ops,
        }
    }
}

/// Per-mount sampling state, owned by the monitor thread (`runtime::run`).
pub(crate) struct Sampler {
    /// Pool name written into each status.
    pool: String,
    /// Mount start, Unix seconds (for uptime).
    started_unix: u64,
    /// Pool remote addresses, in status order.
    remotes: Vec<String>,
    /// Backend type per remote; `None` until `set_backends`.
    backends: Vec<Option<String>>,
    /// First time each pending item was seen, Unix seconds.
    first_seen: HashMap<String, u64>,
    /// Alert rule state across samples.
    alerts: AlertState,
    /// Start of the minute currently accumulating; `None` before the first sample.
    minute: Option<u64>,
    /// Totals at the start of that minute, per remote.
    minute_base: Vec<Totals>,
    /// Totals of the previous sample, per remote.
    last: Vec<Totals>,
}

impl Sampler {
    /// Empty sampler for `pool`'s `remotes`; `started_unix` is the mount start.
    pub(crate) fn new(pool: &str, remotes: Vec<String>, started_unix: u64) -> Self {
        Self {
            pool: pool.to_owned(),
            started_unix,
            backends: vec![None; remotes.len()],
            remotes,
            first_seen: HashMap::new(),
            alerts: AlertState::default(),
            minute: None,
            minute_base: Vec::new(),
            last: Vec::new(),
        }
    }
    /// Pool remotes, in the order `sample` expects traffic.
    pub(crate) fn remotes(&self) -> &[String] {
        &self.remotes
    }
    /// Stores the backend types (ignored when the length does not match).
    pub(crate) fn set_backends(&mut self, backends: Vec<Option<String>>) {
        if backends.len() == self.remotes.len() {
            self.backends = backends;
        }
    }
    /// True once any backend type is known (stops the lookup retries).
    pub(crate) fn has_backends(&self) -> bool {
        self.backends.iter().any(Option::is_some)
    }

    /// One sample. `traffic` is in [`Self::remotes`] order. Returns the
    /// status and the history points of minutes that ended since the last
    /// sample.
    pub(crate) fn sample(
        &mut self,
        now: u64,
        traffic: &[Traffic],
        pending: &[PendingItem],
        last_sync_unix: Option<u64>,
        mtime: impl Fn(&std::path::Path) -> Option<u64>,
    ) -> (NetStatus, Vec<HistoryPoint>) {
        let queue = self.queue(now, pending, mtime);
        let pairs: Vec<(&str, &Traffic)> = self
            .remotes
            .iter()
            .map(String::as_str)
            .zip(traffic.iter())
            .collect();
        let alerts = self.alerts.evaluate(now, &pairs, queue.pending_files);
        let totals: Vec<Totals> = traffic.iter().map(Totals::of).collect();
        let minute = now / 60 * 60;
        let mut points = Vec::new();
        match self.minute {
            None => {
                // Traffic before the first sample (pre-mount sync) belongs
                // to the first minute, like it belongs to the totals.
                self.minute = Some(minute);
                self.minute_base = vec![Totals::default(); totals.len()];
            }
            Some(current) if current != minute => {
                points = self.points(current, &totals);
                self.minute = Some(minute);
                self.minute_base = totals.clone();
            }
            Some(_) => {}
        }
        self.last = totals;
        let remotes = self
            .remotes
            .iter()
            .zip(&self.backends)
            .zip(traffic)
            .map(|((remote, backend), t)| RemoteTraffic {
                remote: remote.clone(),
                backend: backend.clone(),
                sent_bytes: t.sent_bytes,
                acked_bytes: t.acked_bytes,
                verified_bytes: t.verified_bytes,
                received_bytes: t.received_bytes,
                upload_rate_1s: t.upload_rate_1s,
                upload_rate_10s: t.upload_rate_10s,
                download_rate_1s: t.download_rate_1s,
                download_rate_10s: t.download_rate_10s,
                active_uploads: t.active_uploads,
                active_downloads: t.active_downloads,
                ok_ops: t.ok_ops,
                failed_ops: t.failed_ops,
                retries: t.retries,
                last_ok_unix: t.last_ok_unix,
                last_error: t.last_error.clone(),
                last_error_unix: t.last_error_unix,
            })
            .collect();
        let status = NetStatus {
            version: STATUS_VERSION,
            pool: self.pool.clone(),
            updated_unix: now,
            uptime_seconds: now.saturating_sub(self.started_unix),
            remotes,
            queue,
            last_sync_unix,
            alerts,
        };
        (status, points)
    }

    /// History points of the unfinished minute (on stop), from the totals of
    /// the last sample.
    pub(crate) fn flush(&mut self) -> Vec<HistoryPoint> {
        let Some(minute) = self.minute else {
            return Vec::new();
        };
        let last = self.last.clone();
        let points = self.points(minute, &last);
        self.minute_base = last;
        points
    }

    /// Per-remote deltas since the minute start; remotes with no activity are
    /// left out.
    fn points(&self, minute: u64, totals: &[Totals]) -> Vec<HistoryPoint> {
        self.remotes
            .iter()
            .zip(totals)
            .enumerate()
            .filter_map(|(at, (remote, now))| {
                let base = self.minute_base.get(at).copied().unwrap_or_default();
                let point = HistoryPoint {
                    minute_unix: minute,
                    remote: remote.clone(),
                    upload_bytes: now.upload.saturating_sub(base.upload),
                    download_bytes: now.download.saturating_sub(base.download),
                    ok_ops: now.ok.saturating_sub(base.ok),
                    failed_ops: now.failed.saturating_sub(base.failed),
                };
                (point.upload_bytes + point.download_bytes + point.ok_ops + point.failed_ops > 0)
                    .then_some(point)
            })
            .collect()
    }

    /// Queue summary; each item is dated by its first sighting (or its spool
    /// file's mtime) to find the oldest pending change.
    fn queue(
        &mut self,
        now: u64,
        pending: &[PendingItem],
        mtime: impl Fn(&std::path::Path) -> Option<u64>,
    ) -> QueueStatus {
        let mut seen = HashMap::with_capacity(pending.len());
        for item in pending {
            let since = self.first_seen.get(&item.id).copied().unwrap_or_else(|| {
                item.spool
                    .as_deref()
                    .and_then(&mtime)
                    .map_or(now, |at| at.min(now))
            });
            seen.insert(item.id.clone(), since);
        }
        self.first_seen = seen;
        QueueStatus {
            pending_files: pending.len() as u64,
            pending_bytes: pending.iter().map(|item| item.size).sum(),
            oldest_pending_unix: self.first_seen.values().min().copied(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::model::AlertKind;

    fn traffic(sent: u64, received: u64, ok: u64) -> Traffic {
        Traffic {
            sent_bytes: sent,
            acked_bytes: sent,
            received_bytes: received,
            ok_ops: ok,
            upload_rate_10s: 1.5,
            ..Traffic::default()
        }
    }

    #[test]
    fn status_lists_every_remote_and_the_queue() {
        let mut sampler = Sampler::new("p", vec!["a:".into(), "b:x".into()], 900);
        sampler.set_backends(vec![Some("dropbox".into()), None]);
        let pending = [
            PendingItem {
                id: "i1".into(),
                size: 10,
                spool: Some("/spool/i1".into()),
            },
            PendingItem {
                id: "i2".into(),
                size: 5,
                spool: None,
            },
        ];
        let (status, points) = sampler.sample(
            1000,
            &[traffic(7, 3, 1), Traffic::default()],
            &pending,
            Some(950),
            |_| Some(800),
        );
        assert!(points.is_empty());
        assert_eq!(status.uptime_seconds, 100);
        assert_eq!(status.last_sync_unix, Some(950));
        assert_eq!(
            status.queue,
            QueueStatus {
                pending_files: 2,
                pending_bytes: 15,
                oldest_pending_unix: Some(800),
            }
        );
        assert_eq!(status.remotes.len(), 2);
        let a = &status.remotes[0];
        assert_eq!(
            (
                a.remote.as_str(),
                a.backend.as_deref(),
                a.sent_bytes,
                a.acked_bytes
            ),
            ("a:", Some("dropbox"), 7, 7)
        );
        assert_eq!((a.received_bytes, a.ok_ops, a.upload_rate_10s), (3, 1, 1.5));
        assert_eq!(status.remotes[1].sent_bytes, 0);
        // First-seen times stick; finished items leave the queue.
        let (status, _) = sampler.sample(
            1001,
            &[traffic(7, 3, 1), Traffic::default()],
            &pending[1..],
            None,
            |_| None,
        );
        assert_eq!(status.queue.oldest_pending_unix, Some(1000));
        assert_eq!(status.queue.pending_bytes, 5);
    }

    #[test]
    fn minutes_become_history_points_for_remotes_with_traffic() {
        let mut sampler = Sampler::new("p", vec!["a:".into(), "b:".into()], 0);
        let idle = Traffic::default();
        sampler.sample(6000, &[traffic(100, 0, 1), idle.clone()], &[], None, |_| {
            None
        });
        let (_, points) = sampler.sample(
            6030,
            &[traffic(150, 20, 3), idle.clone()],
            &[],
            None,
            |_| None,
        );
        assert!(points.is_empty());
        let (_, points) = sampler.sample(
            6061,
            &[traffic(400, 20, 4), idle.clone()],
            &[],
            None,
            |_| None,
        );
        assert_eq!(
            points,
            [HistoryPoint {
                minute_unix: 6000,
                remote: "a:".into(),
                upload_bytes: 400,
                download_bytes: 20,
                ok_ops: 4,
                failed_ops: 0,
            }]
        );
        let flushed = sampler.flush();
        assert_eq!(flushed.len(), 0, "no traffic since 6060 sample");
        sampler.sample(6070, &[traffic(401, 20, 4), idle], &[], None, |_| None);
        let flushed = sampler.flush();
        assert_eq!((flushed[0].minute_unix, flushed[0].upload_bytes), (6060, 1));
    }

    #[test]
    fn stalled_queue_is_reported() {
        let mut sampler = Sampler::new("p", vec!["a:".into()], 0);
        let pending = [PendingItem {
            id: "x".into(),
            size: 1,
            spool: None,
        }];
        let t = [traffic(5, 0, 0)];
        sampler.sample(100, &t, &pending, None, |_| None);
        let (status, _) = sampler.sample(
            100 + crate::monitor::model::STALL_SECONDS,
            &t,
            &pending,
            None,
            |_| None,
        );
        assert_eq!(status.alerts.len(), 1);
        assert_eq!(status.alerts[0].kind, AlertKind::Stalled);
    }
}
