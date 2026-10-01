use super::format;
use super::history::{aggregate, range_start, HistoryState, Range, HISTORY_REFRESH};
use super::ring::{unit_points, Ring, Sample, LIVE_WINDOW_SECONDS};
use super::sample;
use super::state::{MonitoringState, STATUS_EVERY};
use super::view_model::{activity, alert_text, mount_view, Activity};
use crate::monitor::model::{Alert, AlertKind, HistoryPoint};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NOW: u64 = 1_750_000_000;

fn sample_at(unix: u64, up: f64, down: f64) -> Sample {
    Sample { unix, up, down }
}

fn point(minute_unix: u64, remote: &str, up: u64, down: u64) -> HistoryPoint {
    HistoryPoint {
        minute_unix,
        remote: remote.into(),
        upload_bytes: up,
        download_bytes: down,
        ok_ops: 1,
        failed_ops: 0,
    }
}

#[test]
fn ring_ignores_repeated_statuses_and_keeps_ten_minutes() {
    let mut ring = Ring::default();
    ring.push(sample_at(100, 1.0, 0.0));
    ring.push(sample_at(100, 9.0, 0.0)); // unchanged status file
    ring.push(sample_at(99, 9.0, 0.0)); // older
    assert_eq!(ring.len(), 1);
    for t in 101..=100 + LIVE_WINDOW_SECONDS + 50 {
        ring.push(sample_at(t, 2.0, 3.0));
    }
    let first = ring.samples().next().unwrap().unix;
    assert_eq!(first, 150, "samples older than the window are dropped");
    assert_eq!(ring.len() as u64, LIVE_WINDOW_SECONDS + 1);
    assert_eq!(ring.peak(), 3.0);
}

#[test]
fn sparkline_points_scale_to_window_and_peak() {
    let mut ring = Ring::default();
    ring.push(sample_at(NOW - LIVE_WINDOW_SECONDS - 10, 50.0, 0.0)); // outside
    ring.push(sample_at(NOW - LIVE_WINDOW_SECONDS, 0.0, 0.0));
    ring.push(sample_at(NOW - LIVE_WINDOW_SECONDS / 2, 50.0, 0.0));
    ring.push(sample_at(NOW, 100.0, 0.0));
    let points = unit_points(&ring, NOW, 100.0, |s| s.up);
    assert_eq!(points, vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
    // A zero peak keeps every point on the baseline (no division by zero).
    let flat = unit_points(&ring, NOW, 0.0, |s| s.up);
    assert!(flat.iter().all(|[_, y]| *y == 0.0));
    // Values above the given peak are clamped.
    let clamped = unit_points(&ring, NOW, 10.0, |s| s.up);
    assert_eq!(clamped.last().unwrap()[1], 1.0);
}

#[test]
fn every_range_has_its_bucket_size_and_count() {
    let expected = [
        (Range::Hour, 60, 60, 3_600),
        (Range::Day, 900, 96, 86_400),
        (Range::Week, 3_600, 168, 7 * 86_400),
        (Range::Month, 21_600, 120, 30 * 86_400),
    ];
    for (range, size, count, seconds) in expected {
        assert_eq!(range.bucket_seconds(), size, "{range:?}");
        assert_eq!(range.buckets(), count, "{range:?}");
        assert_eq!(range.seconds(), seconds, "{range:?}");
        let start = range_start(range, NOW);
        assert_eq!(start % size, 0, "{range:?} buckets are aligned");
        assert!(start + seconds > NOW && start + seconds - size <= NOW);
    }
}

#[test]
fn hour_buckets_are_per_minute_and_skip_outside_points() {
    let start = range_start(Range::Hour, NOW);
    let points = vec![
        point(start, "b:", 10, 1),
        point(start, "a:", 5, 0),
        point(start + 60, "a:", 7, 2),
        point(start + 59 * 60, "a:", 3, 0),
        point(start - 60, "a:", 1_000, 0),    // before the range
        point(start + 3_600, "a:", 1_000, 0), // after the last bucket
    ];
    let chart = aggregate(&points, Range::Hour, NOW);
    assert_eq!(chart.start_unix, start);
    let names: Vec<&str> = chart.remotes.iter().map(|r| r.remote.as_str()).collect();
    assert_eq!(names, ["a:", "b:"], "sorted by account");
    let a = &chart.remotes[0];
    assert_eq!(a.buckets.len(), 60);
    assert_eq!(
        (a.buckets[0].up, a.buckets[1].up, a.buckets[59].up),
        (5, 7, 3)
    );
    assert_eq!((a.total_up, a.total_down, a.ok_ops), (15, 2, 3));
    assert_eq!(a.peak(), 7);
    assert_eq!(chart.remotes[1].total_up, 10);
}

#[test]
fn longer_ranges_sum_minutes_into_their_buckets() {
    for range in [Range::Day, Range::Week, Range::Month] {
        let start = range_start(range, NOW);
        let size = range.bucket_seconds();
        // Every minute of the first two buckets: 1 byte up, 2 down each.
        let points: Vec<HistoryPoint> = (0..2 * size / 60)
            .map(|m| point(start + m * 60, "x:", 1, 2))
            .collect();
        let chart = aggregate(&points, range, NOW);
        let row = &chart.remotes[0];
        let per_bucket = size / 60;
        assert_eq!(row.buckets.len(), range.buckets(), "{range:?}");
        assert_eq!(row.buckets[0].up, per_bucket, "{range:?}");
        assert_eq!(row.buckets[1].down, 2 * per_bucket, "{range:?}");
        assert_eq!(row.buckets[2].up, 0, "{range:?}");
        assert_eq!(row.total_up, 2 * per_bucket, "{range:?}");
    }
}

#[test]
fn rates_bytes_and_durations_are_formatted() {
    assert_eq!(format::rate(0.0), "0 B/s");
    assert_eq!(format::rate(820.0), "820 B/s");
    assert_eq!(format::rate(1_500.0), "1.5 KB/s");
    assert_eq!(format::rate(12_340_000.0), "12.3 MB/s");
    assert_eq!(format::rate(1_100_000_000.0), "1.1 GB/s");
    assert_eq!(format::rate(-5.0), "0 B/s");
    assert_eq!(format::rate(f64::NAN), "0 B/s");
    assert_eq!(format::bytes(512), "512 B");
    assert_eq!(format::bytes(3 * 1024 * 1024), "3.00 MiB");
    assert_eq!(format::duration(45), "45s");
    assert_eq!(format::duration(12 * 60 + 5), "12m 5s");
    assert_eq!(format::duration(3 * 3_600 + 20 * 60 + 7), "3h 20m");
    assert_eq!(format::duration(2 * 86_400 + 4 * 3_600), "2d 4h");
}

#[test]
fn alerts_show_translated_text_by_kind() {
    let alert = |kind, remote: Option<&str>| Alert {
        kind,
        remote: remote.map(str::to_string),
        since_unix: NOW,
        message: "english message from the file".into(),
    };
    assert_eq!(
        alert_text(&alert(AlertKind::Stalled, None)),
        "Uploads are waiting but nothing has been sent for 3 minutes"
    );
    assert_eq!(
        alert_text(&alert(AlertKind::Errors, None)),
        "Errors increased in the last minute"
    );
    assert_eq!(
        alert_text(&alert(AlertKind::Errors, Some("gd:"))),
        "gd:: errors increased in the last minute"
    );
    assert_eq!(
        alert_text(&alert(AlertKind::Unreachable, Some("gd:"))),
        "gd: has not answered for a minute"
    );
    assert_eq!(
        alert_text(&alert(AlertKind::Unreachable, None)),
        "An account has not answered for a minute"
    );
}

#[test]
fn uploading_wins_over_downloading_and_idle_needs_no_traffic() {
    let status = sample::family_status(NOW);
    let mut remote = status.remotes[0].clone();
    remote.upload_rate_1s = 0.0;
    remote.active_uploads = 0;
    remote.download_rate_1s = 0.0;
    remote.active_downloads = 0;
    assert_eq!(activity(&remote), Activity::Idle);
    // The 10 s average alone (traffic that just stopped) is not "now".
    remote.upload_rate_10s = 5_000.0;
    assert_eq!(activity(&remote), Activity::Idle);
    remote.active_downloads = 1;
    assert_eq!(activity(&remote), Activity::Downloading);
    remote.active_uploads = 1;
    assert_eq!(activity(&remote), Activity::Uploading);
    remote.active_uploads = 0;
    remote.upload_rate_1s = 1.0;
    assert_eq!(activity(&remote), Activity::Uploading);
}

#[test]
fn view_model_from_sample_status() {
    let entry = sample::entry("a1", "family", "/Volumes/family", "fuse");
    let status = sample::family_status(NOW);
    let view = mount_view(&entry, Some(&status), NOW);
    assert_eq!(view.frontend, "FUSE");
    let live = view.live.unwrap();
    assert_eq!(live.uptime, "Up 5h 12m");
    assert_eq!(live.last_sync, "Last sync 1m ago");
    assert_eq!(live.queue, "37 files, 782.01 MiB waiting");
    assert_eq!(live.queue_oldest.as_deref(), Some("oldest since 4m 20s"));
    assert_eq!(live.activity, Activity::Uploading);
    assert_eq!(live.alerts.len(), 2);
    assert_eq!(live.alerts[0].0, AlertKind::Unreachable);
    let dropbox = &live.remotes[0];
    assert_eq!(dropbox.upload_rate, "4.2 MB/s");
    assert_eq!(dropbox.download_rate, "0 B/s");
    assert_eq!(dropbox.transfers, "3 uploads and 0 downloads running");
    assert_eq!(dropbox.ops, "1204 OK · 0 failed · 7 retries");
    assert_eq!(dropbox.last_ok, "Last OK now");
    assert!(dropbox
        .totals
        .starts_with("Sent 3.17 GiB · acknowledged 2.89 GiB"));
    assert_eq!(dropbox.last_error, None);
    let onedrive = &live.remotes[2];
    assert_eq!(onedrive.activity, Activity::Idle);
    assert_eq!(
        onedrive.last_error.as_deref(),
        Some("upload failed: connection timed out after 30s (now)")
    );
    assert_eq!(onedrive.last_ok, "Last OK 25m ago");
    // No status yet: the card waits instead of showing zeros.
    assert!(mount_view(&entry, None, NOW).live.is_none());
    // An empty queue and a never-synced pool.
    let mut idle = sample::photos_status(NOW);
    idle.queue = Default::default();
    idle.last_sync_unix = None;
    let live = mount_view(&entry, Some(&idle), NOW).live.unwrap();
    assert_eq!(live.queue, "Nothing waiting to upload");
    assert_eq!(live.queue_oldest, None);
    assert_eq!(live.last_sync, "Not synced yet");
    assert_eq!(live.activity, Activity::Idle);
}

#[test]
fn polling_reads_mounts_and_status_on_their_intervals_and_keeps_samples() {
    let source = Arc::new(sample::source(NOW));
    let mut state = MonitoringState::with_source(source);
    let start = Instant::now();
    state.poll(start);
    assert_eq!(state.mounts.len(), 2);
    assert!(state.mounts.iter().all(|m| m.status.is_some()));
    assert_eq!(state.mounts[0].rings.len(), 3);
    // A second status of the same second adds no sample.
    state.poll(start + STATUS_EVERY);
    assert_eq!(state.mounts[0].rings.values().next().unwrap().len(), 1);
    // A newer status adds one, and survives re-reading the registry.
    let mut newer = state.mounts[0].status.clone().unwrap();
    newer.updated_unix += 1;
    state.mounts[0].record(Some(newer));
    let entries: Vec<_> = state.mounts.iter().map(|m| m.entry.clone()).collect();
    state.reconcile(entries.clone());
    assert_eq!(state.mounts[0].rings.values().next().unwrap().len(), 2);
    // An ended mount disappears.
    state.reconcile(entries[1..].to_vec());
    assert_eq!(state.mounts.len(), 1);
    assert_eq!(state.mounts[0].entry.pool, "photos");
    assert!(!state.busy(), "photos is idle");
}

#[test]
fn history_loads_in_background_and_refreshes_after_a_minute() {
    let source = Arc::new(sample::source(NOW));
    let mut history = HistoryState::default();
    let start = Instant::now();
    assert!(history.needs_load(Range::Hour, start));
    history.start(source, "/fake/workspaces/family".into(), Range::Hour, NOW);
    assert!(!history.needs_load(Range::Hour, start), "already loading");
    assert!(history.needs_load(Range::Day, start), "another range");
    let deadline = Instant::now() + Duration::from_secs(10);
    while history.loading() && Instant::now() < deadline {
        history.poll(start);
        std::thread::sleep(Duration::from_millis(5));
    }
    let chart = history.chart.as_ref().expect("loaded");
    assert_eq!(chart.range, Range::Hour);
    assert_eq!(chart.remotes.len(), 3);
    assert!(chart.remotes.iter().all(|r| r.total_up + r.total_down > 0));
    assert!(!history.needs_load(Range::Hour, start + Duration::from_secs(30)));
    assert!(history.needs_load(Range::Hour, start + HISTORY_REFRESH));
}

#[test]
fn sample_fixture_has_two_mounts_with_live_graphs_and_history() {
    let state = sample::state(NOW, true);
    assert_eq!(state.mounts.len(), 2);
    assert!(state.busy());
    let ring = state.mounts[0].rings.values().next().unwrap();
    assert!(ring.len() > 100 && ring.peak() > 0.0);
    let chart = state.mounts[0].history.chart.as_ref().unwrap();
    assert_eq!(chart.range, Range::Day);
    assert_eq!(chart.remotes.len(), 3);
}
