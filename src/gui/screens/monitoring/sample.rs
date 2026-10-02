//! Realistic fake monitoring data for tests, layout fixtures and debug
//! snapshots (`RPOOL_GUI_SNAPSHOT_MONITOR=1`): two mounted pools, one busy
//! with an unreachable account, one with a stalled queue.
use super::history::Range;
use super::ring::{Sample, LIVE_WINDOW_SECONDS};
use super::source::FixedSource;
use super::state::{CardTab, MonitoringState};
use crate::monitor::model::*;
use std::sync::Arc;
use std::time::Instant;

/// An idle `RemoteTraffic` of `name` on `backend`; callers fill in the traffic.
fn remote(name: &str, backend: &str) -> RemoteTraffic {
    RemoteTraffic {
        remote: name.into(),
        backend: Some(backend.into()),
        sent_bytes: 0,
        acked_bytes: 0,
        verified_bytes: 0,
        received_bytes: 0,
        upload_rate_1s: 0.0,
        upload_rate_10s: 0.0,
        download_rate_1s: 0.0,
        download_rate_10s: 0.0,
        active_uploads: 0,
        active_downloads: 0,
        ok_ops: 0,
        failed_ops: 0,
        retries: 0,
        last_ok_unix: None,
        last_error: None,
        last_error_unix: None,
    }
}

/// A registry entry of a fake mount of `pool` at `mountpoint` via `frontend`.
pub(crate) fn entry(id: &str, pool: &str, mountpoint: &str, frontend: &str) -> MountEntry {
    MountEntry {
        id: id.into(),
        pool: pool.into(),
        workspace: format!("/fake/workspaces/{pool}"),
        mountpoint: mountpoint.into(),
        frontend: frontend.into(),
        pid: 4242,
        started_unix: 1_700_000_000,
    }
}

/// The "family" pool: one account uploading, one downloading, one failing.
pub(crate) fn family_status(now: u64) -> NetStatus {
    let mut dropbox = remote("dropbox_1_crypt:rpool", "dropbox");
    dropbox.sent_bytes = 3_400_000_000;
    dropbox.acked_bytes = 3_100_000_000;
    dropbox.verified_bytes = 2_900_000_000;
    dropbox.received_bytes = 210_000_000;
    dropbox.upload_rate_1s = 4_800_000.0;
    dropbox.upload_rate_10s = 4_200_000.0;
    dropbox.active_uploads = 3;
    dropbox.ok_ops = 1_204;
    dropbox.retries = 7;
    dropbox.last_ok_unix = Some(now - 2);

    let mut drive = remote("google_drive_backup_account_2_crypt:rpool", "drive");
    drive.sent_bytes = 1_250_000_000;
    drive.acked_bytes = 1_250_000_000;
    drive.verified_bytes = 1_250_000_000;
    drive.received_bytes = 880_000_000;
    drive.download_rate_1s = 950_000.0;
    drive.download_rate_10s = 1_100_000.0;
    drive.active_downloads = 1;
    drive.ok_ops = 860;
    drive.last_ok_unix = Some(now - 5);

    let mut onedrive = remote("onedrive_old_crypt:rpool", "onedrive");
    onedrive.sent_bytes = 400_000_000;
    onedrive.acked_bytes = 380_000_000;
    onedrive.verified_bytes = 380_000_000;
    onedrive.ok_ops = 310;
    onedrive.failed_ops = 42;
    onedrive.retries = 18;
    onedrive.last_ok_unix = Some(now - 1_500);
    onedrive.last_error = Some("upload failed: connection timed out after 30s".into());
    onedrive.last_error_unix = Some(now - 20);

    NetStatus {
        version: STATUS_VERSION,
        pool: "family".into(),
        updated_unix: now,
        uptime_seconds: 5 * 3_600 + 12 * 60,
        remotes: vec![dropbox, drive, onedrive],
        queue: QueueStatus {
            pending_files: 37,
            pending_bytes: 820_000_000,
            oldest_pending_unix: Some(now - 260),
        },
        last_sync_unix: Some(now - 90),
        alerts: vec![
            Alert {
                kind: AlertKind::Unreachable,
                remote: Some("onedrive_old_crypt:rpool".into()),
                since_unix: now - 80,
                message: "onedrive_old_crypt:rpool has not answered for 80 s".into(),
            },
            Alert {
                kind: AlertKind::Errors,
                remote: Some("onedrive_old_crypt:rpool".into()),
                since_unix: now - 40,
                message: "failed operations increased".into(),
            },
            Alert {
                kind: AlertKind::UploadLimit,
                remote: Some("google_drive_backup_account_2_crypt:rpool".into()),
                since_unix: now - 600,
                message: "google_drive_backup_account_2_crypt:rpool: account google_drive_backup_account_2 reached its daily upload limit; uploads resume in 3 h 10 min".into(),
            },
        ],
    }
}

/// The "photos" pool: uploads pending but nothing sent (stalled), idle.
pub(crate) fn photos_status(now: u64) -> NetStatus {
    let mut nas = remote("nas_sftp_crypt:photos", "sftp");
    nas.sent_bytes = 12_000_000;
    nas.acked_bytes = 12_000_000;
    nas.received_bytes = 4_000_000;
    nas.ok_ops = 22;
    nas.last_ok_unix = Some(now - 400);
    let mut pcloud = remote("pcloud_crypt:photos", "pcloud");
    pcloud.ok_ops = 3;
    pcloud.last_ok_unix = Some(now - 4_000);
    NetStatus {
        version: STATUS_VERSION,
        pool: "photos".into(),
        updated_unix: now,
        uptime_seconds: 2 * 86_400 + 3 * 3_600,
        remotes: vec![nas, pcloud],
        queue: QueueStatus {
            pending_files: 4,
            pending_bytes: 96_000_000,
            oldest_pending_unix: Some(now - 600),
        },
        last_sync_unix: Some(now - 7_200),
        alerts: vec![Alert {
            kind: AlertKind::Stalled,
            remote: None,
            since_unix: now - 200,
            message: "uploads pending, nothing sent for 180 s".into(),
        }],
    }
}

/// A smooth daily pattern of per-minute history for the last 30 days.
pub(crate) fn history(remotes: &[&str], now: u64) -> Vec<HistoryPoint> {
    let mut points = Vec::new();
    let first = (now / 60).saturating_sub(30 * 1_440);
    // Every minute of the last day, every 7th before (fewer points).
    let recent = (now / 60).saturating_sub(1_440);
    let minutes = (first..recent).step_by(7).chain(recent..=now / 60);
    for minute in minutes {
        for (index, remote) in remotes.iter().enumerate() {
            let phase = (minute as f64 / 240.0 + index as f64).sin().max(0.0);
            points.push(HistoryPoint {
                minute_unix: minute * 60,
                remote: (*remote).into(),
                upload_bytes: (phase * 90_000_000.0 / (index + 1) as f64) as u64,
                download_bytes: ((1.0 - phase) * 20_000_000.0) as u64,
                ok_ops: 4,
                failed_ops: u64::from(index == 2 && minute % 5 == 0),
            });
        }
    }
    points
}

/// A `FixedSource` with the two sample mounts, their statuses at `now` and
/// their minute history.
pub(crate) fn source(now: u64) -> FixedSource {
    let family = family_status(now);
    let photos = photos_status(now);
    let family_remotes: Vec<&str> = family.remotes.iter().map(|r| r.remote.as_str()).collect();
    let photos_remotes: Vec<&str> = photos.remotes.iter().map(|r| r.remote.as_str()).collect();
    FixedSource {
        mounts: vec![
            (
                entry("a1", "family", "/Volumes/family", "fuse"),
                Some(family.clone()),
                history(&family_remotes, now),
            ),
            (
                entry("b2", "photos", "R:", "winfsp"),
                Some(photos.clone()),
                history(&photos_remotes, now),
            ),
        ],
    }
}

/// A polled state of [`source`] with 10 minutes of varied live samples; with
/// `history` the first card shows its 24 h history (loaded synchronously).
pub(crate) fn state(now_unix: u64, history: bool) -> MonitoringState {
    let mut state = MonitoringState::with_source(Arc::new(source(now_unix)));
    let now = Instant::now();
    state.poll(now);
    for mount in &mut state.mounts {
        for (index, ring) in mount.rings.values_mut().enumerate() {
            *ring = Default::default();
            for t in (now_unix - LIVE_WINDOW_SECONDS..=now_unix).step_by(5) {
                let wave = ((t as f64) / 40.0 + index as f64).sin().max(0.0);
                ring.push(Sample {
                    unix: t,
                    up: wave * 4_000_000.0 / (index + 1) as f64,
                    down: (1.0 - wave) * 600_000.0,
                });
            }
        }
    }
    if history {
        if let Some(first) = state.mounts.first_mut() {
            first.tab = CardTab::History;
            first.range = Range::Day;
        }
        state.load_history_now(now, now_unix);
    }
    state
}
