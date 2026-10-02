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

/// Time span of a mount card's history chart; picked in `history_view::show`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Range {
    /// Last hour, one bar per minute (the default).
    #[default]
    Hour,
    /// Last 24 hours, one bar per 15 minutes.
    Day,
    /// Last 7 days, one bar per hour.
    Week,
    /// Last 30 days, one bar per 6 hours.
    Month,
}

impl Range {
    /// Every range, in the order of the picker.
    pub(crate) const ALL: [Range; 4] = [Range::Hour, Range::Day, Range::Week, Range::Month];

    /// Length of one bar (bucket) in seconds.
    pub(crate) fn bucket_seconds(self) -> u64 {
        match self {
            Range::Hour => 60,
            Range::Day => 15 * 60,
            Range::Week => 3_600,
            Range::Month => 6 * 3_600,
        }
    }

    /// Number of bars in the chart.
    pub(crate) fn buckets(self) -> usize {
        match self {
            Range::Hour => 60,
            Range::Day => 96,
            Range::Week => 168,
            Range::Month => 120,
        }
    }

    /// Whole span of the range in seconds (bucket length × count).
    pub(crate) fn seconds(self) -> u64 {
        self.bucket_seconds() * self.buckets() as u64
    }
}

/// Traffic of one account in one bucket, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Bucket {
    /// Bytes uploaded.
    pub up: u64,
    /// Bytes downloaded.
    pub down: u64,
}

/// One account's row of a history chart, built by `aggregate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteHistory {
    /// Remote (account) name.
    pub remote: String,
    /// `Range::buckets` buckets, oldest first.
    pub buckets: Vec<Bucket>,
    /// Bytes uploaded in the whole range.
    pub total_up: u64,
    /// Bytes downloaded in the whole range.
    pub total_down: u64,
    /// Successful operations in the range.
    pub ok_ops: u64,
    /// Failed operations in the range.
    pub failed_ops: u64,
}

impl RemoteHistory {
    /// Largest upload or download of any bucket, for scaling the bars.
    pub(crate) fn peak(&self) -> u64 {
        self.buckets
            .iter()
            .map(|b| b.up.max(b.down))
            .max()
            .unwrap_or(0)
    }
}

/// A mount's traffic history for one range, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryChart {
    /// Range the chart covers.
    pub range: Range,
    /// Start of the first bucket.
    pub start_unix: u64,
    /// One row per account, sorted by name.
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
    /// Last finished chart (kept on screen while a reload runs).
    pub chart: Option<HistoryChart>,
    /// Range and time of the last finished load.
    loaded: Option<(Range, Instant)>,
    /// Range and receiver of a load still running on a background thread.
    pending: Option<(Range, Receiver<HistoryChart>)>,
}

impl HistoryState {
    /// Whether a load is running.
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
