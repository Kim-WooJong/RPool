//! Human text of `rpool mount monitor` (the GUI Monitoring page shows the
//! same fields).
use super::model::{Alert, AlertKind, MountEntry, NetStatus};
use crate::presentation::format_bytes;

pub(crate) fn duration(seconds: u64) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
    );
    if d > 0 {
        format!("{d}d{h:02}h")
    } else if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

fn ago(at: Option<u64>, now: u64) -> String {
    match at {
        Some(at) => format!("{} ago", duration(now.saturating_sub(at))),
        None => "never".into(),
    }
}

pub(crate) fn rate(bytes_per_second: f64) -> String {
    format!(
        "{}/s",
        format_bytes(bytes_per_second.max(0.0).round() as u64)
    )
}

fn alert_label(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Stalled => "STALLED",
        AlertKind::Errors => "ERRORS",
        AlertKind::Unreachable => "UNREACHABLE",
        AlertKind::MetadataGrowing => "METADATA",
        AlertKind::UploadLimit => "UPLOAD LIMIT",
    }
}

fn alert_line(alert: &Alert, now: u64) -> String {
    format!(
        "  [{}] {} (since {})",
        alert_label(alert.kind),
        alert.message,
        ago(Some(alert.since_unix), now)
    )
}

/// One mount: header, queue, alerts and the per-remote table.
pub(crate) fn mount(entry: &MountEntry, status: Option<&NetStatus>, now: u64) -> String {
    let mut out = format!(
        "Pool {}  at {}  ({}, pid {})\n",
        entry.pool, entry.mountpoint, entry.frontend, entry.pid
    );
    let Some(status) = status else {
        out.push_str(&format!(
            "  started {}; no live status (mount starting, hung or from an older RPool)\n",
            ago(Some(entry.started_unix), now)
        ));
        return out;
    };
    let queue = &status.queue;
    out.push_str(&format!(
        "  uptime {}  queue {} file(s) / {}{}  last sync {}\n",
        duration(status.uptime_seconds),
        queue.pending_files,
        format_bytes(queue.pending_bytes),
        queue
            .oldest_pending_unix
            .map(|at| format!(" (oldest {})", ago(Some(at), now)))
            .unwrap_or_default(),
        ago(status.last_sync_unix, now),
    ));
    if status.alerts.is_empty() {
        out.push_str("  alerts: none\n");
    } else {
        for alert in &status.alerts {
            out.push_str(&alert_line(alert, now));
            out.push('\n');
        }
    }
    let header = [
        "REMOTE",
        "BACKEND",
        "UP 10s",
        "DOWN 10s",
        "SENT",
        "ACKED",
        "VERIFIED",
        "RECEIVED",
        "ACTIVE",
        "OK/FAIL/RETRY",
        "LAST ERROR",
    ];
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    for r in &status.remotes {
        rows.push(vec![
            r.remote.clone(),
            r.backend.clone().unwrap_or_else(|| "-".into()),
            rate(r.upload_rate_10s),
            rate(r.download_rate_10s),
            format_bytes(r.sent_bytes),
            format_bytes(r.acked_bytes),
            format_bytes(r.verified_bytes),
            format_bytes(r.received_bytes),
            format!("{}↑ {}↓", r.active_uploads, r.active_downloads),
            format!("{}/{}/{}", r.ok_ops, r.failed_ops, r.retries),
            match (&r.last_error, r.last_error_unix) {
                (Some(error), at) => format!("{error} ({})", ago(at, now)),
                (None, _) => "-".into(),
            },
        ]);
    }
    let columns = header.len();
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            rows.iter()
                .map(|row| row[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        let mut line = String::from(" ");
        for (c, cell) in row.iter().enumerate() {
            line.push_str(if c == 0 { " " } else { "  " });
            if c + 1 == columns {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&" ".repeat(widths[c] - cell.chars().count()));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::model::{QueueStatus, RemoteTraffic, STATUS_VERSION};

    #[test]
    fn durations_and_rates_are_compact() {
        assert_eq!(duration(5), "5s");
        assert_eq!(duration(65), "1m05s");
        assert_eq!(duration(3_725), "1h02m");
        assert_eq!(duration(90_000), "1d01h");
        assert_eq!(rate(1536.0), "1.50 KiB/s");
        assert_eq!(rate(-1.0), "0 B/s");
    }

    #[test]
    fn mount_text_shows_header_queue_alerts_and_remote_table() {
        let entry = MountEntry {
            id: "ab".into(),
            pool: "main".into(),
            workspace: "/w".into(),
            mountpoint: "/mnt/main".into(),
            frontend: "dav".into(),
            pid: 42,
            started_unix: 900,
        };
        assert!(mount(&entry, None, 1000).contains("no live status"));
        let status = NetStatus {
            version: STATUS_VERSION,
            pool: "main".into(),
            updated_unix: 1000,
            uptime_seconds: 3_725,
            remotes: vec![RemoteTraffic {
                remote: "dropbox_1_crypt:".into(),
                backend: Some("dropbox".into()),
                sent_bytes: 2 * 1024 * 1024,
                acked_bytes: 1024 * 1024,
                verified_bytes: 1024 * 1024,
                received_bytes: 10,
                upload_rate_1s: 0.0,
                upload_rate_10s: 2048.0,
                download_rate_1s: 0.0,
                download_rate_10s: 0.0,
                active_uploads: 1,
                active_downloads: 0,
                ok_ops: 7,
                failed_ops: 2,
                retries: 1,
                last_ok_unix: Some(990),
                last_error: Some("rclone timed out".into()),
                last_error_unix: Some(970),
            }],
            queue: QueueStatus {
                pending_files: 3,
                pending_bytes: 2048,
                oldest_pending_unix: Some(700),
            },
            last_sync_unix: Some(940),
            alerts: vec![Alert {
                kind: AlertKind::Errors,
                remote: Some("dropbox_1_crypt:".into()),
                since_unix: 970,
                message: "dropbox_1_crypt: operations failed recently".into(),
            }],
        };
        let text = mount(&entry, Some(&status), 1000);
        let expected = [
            "Pool main  at /mnt/main  (dav, pid 42)",
            "uptime 1h02m  queue 3 file(s) / 2.00 KiB (oldest 5m00s ago)  last sync 1m00s ago",
            "[ERRORS] dropbox_1_crypt: operations failed recently (since 30s ago)",
            "REMOTE",
            "dropbox_1_crypt:  dropbox  2.00 KiB/s  0 B/s     2.00 MiB  1.00 MiB  1.00 MiB  10 B",
            "1↑ 0↓   7/2/1          rclone timed out (30s ago)",
        ];
        for part in expected {
            assert!(text.contains(part), "missing {part:?} in\n{text}");
        }
    }
}
