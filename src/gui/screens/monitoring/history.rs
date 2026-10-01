//! Traffic history of one mount: the selectable ranges, aggregation of the
//! per-minute history points into chart buckets, and loading off the UI
//! thread.
use super::source::MonitorSource;
use crate::monitor::model::HistoryPoint;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A loaded range is refreshed after this long while it is shown.
pub(crate) const HISTORY_REFRESH: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Range {
    #[default]
    Hour,
    Day,
    Week,
    Month,
}

impl Range {
    pub(crate) const ALL: [Range; 4] = [Range::Hour, Range::Day, Range::Week, Range::Month];

    pub(crate) fn bucket_seconds(self) -> u64 {
        match self {
            Range::Hour => 60,
            Range::Day => 15 * 60,
            Range::Week => 3_600,
            Range::Month => 6 * 3_600,
        }
    }

    pub(crate) fn buckets(self) -> usize {
        match self {
            Range::Hour => 60,
            Range::Day => 96,
            Range::Week => 168,
            Range::Month => 120,
        }
    }

    pub(crate) fn seconds(self) -> u64 {
        self.bucket_seconds() * self.buckets() as u64
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Bucket {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteHistory {
    pub remote: String,
    pub buckets: Vec<Bucket>,
    pub total_up: u64,
    pub total_down: u64,
    pub ok_ops: u64,
    pub failed_ops: u64,
}

impl RemoteHistory {
    pub(crate) fn peak(&self) -> u64 {
        self.buckets
            .iter()
            .map(|b| b.up.max(b.down))
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryChart {
    pub range: Range,
    /// Start of the first bucket.
    pub start_unix: u64,
    pub remotes: Vec<RemoteHistory>,
}

/// Start of the first bucket so the last bucket holds `now`.
pub(crate) fn range_start(range: Range, now: u64) -> u64 {
    let size = range.bucket_seconds();
    let end = (now / size + 1) * size;
    end.saturating_sub(range.seconds())
}

/// Sums `points` into the buckets of `range` ending at `now`, one row per
/// account sorted by name. Points outside the range are ignored.
pub(crate) fn aggregate(points: &[HistoryPoint], range: Range, now: u64) -> HistoryChart {
    let start = range_start(range, now);
    let size = range.bucket_seconds();
    let count = range.buckets();
    let mut rows: BTreeMap<&str, RemoteHistory> = BTreeMap::new();
    for point in points {
        if point.minute_unix < start {
            continue;
        }
        let index = ((point.minute_unix - start) / size) as usize;
        if index >= count {
            continue;
        }
        let row = rows
            .entry(point.remote.as_str())
            .or_insert_with(|| RemoteHistory {
                remote: point.remote.clone(),
                buckets: vec![Bucket::default(); count],
                total_up: 0,
                total_down: 0,
                ok_ops: 0,
                failed_ops: 0,
            });
        row.buckets[index].up += point.upload_bytes;
        row.buckets[index].down += point.download_bytes;
        row.total_up += point.upload_bytes;
        row.total_down += point.download_bytes;
        row.ok_ops += point.ok_ops;
        row.failed_ops += point.failed_ops;
    }
    HistoryChart {
        range,
        start_unix: start,
        remotes: rows.into_values().collect(),
    }
}

/// The history shown on one card: the last loaded chart and a load in
/// progress.
#[derive(Default)]
pub(crate) struct HistoryState {
    pub chart: Option<HistoryChart>,
    /// Range and time of the last finished load.
    loaded: Option<(Range, Instant)>,
    pending: Option<(Range, Receiver<HistoryChart>)>,
}

impl HistoryState {
    pub(crate) fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether `range` must be (re)loaded at `now`.
    pub(crate) fn needs_load(&self, range: Range, now: Instant) -> bool {
        if let Some((pending, _)) = &self.pending {
            return *pending != range;
        }
        match self.loaded {
            // A failed load also waits for the refresh instead of retrying
            // every frame.
            Some((loaded, at)) => loaded != range || now.duration_since(at) >= HISTORY_REFRESH,
            None => true,
        }
    }

    /// Loads `range` of `workspace` on a background thread.
    pub(crate) fn start(
        &mut self,
        source: Arc<dyn MonitorSource>,
        workspace: PathBuf,
        range: Range,
        now_unix: u64,
    ) {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let since = range_start(range, now_unix);
            let points = source.load_history(&workspace, since);
            let _ = send.send(aggregate(&points, range, now_unix));
        });
        self.pending = Some((range, receive));
    }

    /// Loads synchronously (fixtures and tests).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn load_now(
        &mut self,
        source: &dyn MonitorSource,
        workspace: &std::path::Path,
        range: Range,
        now_unix: u64,
        now: Instant,
    ) {
        let points = source.load_history(workspace, range_start(range, now_unix));
        self.chart = Some(aggregate(&points, range, now_unix));
        self.loaded = Some((range, now));
        self.pending = None;
    }

    /// Takes a finished background load.
    pub(crate) fn poll(&mut self, now: Instant) {
        let Some((range, receive)) = &self.pending else {
            return;
        };
        let range = *range;
        match receive.try_recv() {
            Ok(chart) => {
                self.chart = Some(chart);
                self.loaded = Some((range, now));
                self.pending = None;
            }
            Err(TryRecvError::Disconnected) => {
                self.loaded = Some((range, now));
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }
}
