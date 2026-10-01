//! What one mount card shows, computed from its registry entry and live
//! status at a given time (no egui, so it is unit tested).
use super::format;
use crate::gui::i18n::{relative_age_at, tr, trf};
use crate::monitor::model::{Alert, AlertKind, MountEntry, NetStatus, RemoteTraffic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    Uploading,
    Downloading,
    Idle,
}

/// Uploading wins over downloading: pending uploads are what users wait for.
pub(crate) fn activity(remote: &RemoteTraffic) -> Activity {
    if remote.upload_rate_1s > 0.0 || remote.active_uploads > 0 {
        Activity::Uploading
    } else if remote.download_rate_1s > 0.0 || remote.active_downloads > 0 {
        Activity::Downloading
    } else {
        Activity::Idle
    }
}

pub(crate) fn activity_label(activity: Activity) -> &'static str {
    match activity {
        Activity::Uploading => tr("Uploading"),
        Activity::Downloading => tr("Downloading"),
        Activity::Idle => tr("Idle"),
    }
}

/// The translated banner text of an alert (the English `message` of the
/// file is not shown).
pub(crate) fn alert_text(alert: &Alert) -> String {
    let remote = alert.remote.as_deref();
    match (alert.kind, remote) {
        (AlertKind::Stalled, _) => {
            tr("Uploads are waiting but nothing has been sent for 3 minutes").into()
        }
        (AlertKind::Errors, None) => tr("Errors increased in the last minute").into(),
        (AlertKind::Errors, Some(remote)) => trf(
            "{remote}: errors increased in the last minute",
            &[("remote", &remote)],
        ),
        (AlertKind::Unreachable, Some(remote)) => trf(
            "{remote} has not answered for a minute",
            &[("remote", &remote)],
        ),
        (AlertKind::Unreachable, None) => tr("An account has not answered for a minute").into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteView {
    pub remote: String,
    pub backend: Option<String>,
    pub activity: Activity,
    pub upload_rate: String,
    pub download_rate: String,
    pub totals: String,
    pub transfers: String,
    pub ops: String,
    pub last_ok: String,
    /// Error line and its age.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MountView {
    pub pool: String,
    pub mountpoint: String,
    pub frontend: String,
    /// `None` while no fresh status was read.
    pub live: Option<LiveView>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveView {
    pub uptime: String,
    pub last_sync: String,
    pub queue: String,
    /// Oldest waiting change, when any.
    pub queue_oldest: Option<String>,
    pub queue_waiting: bool,
    pub alerts: Vec<(AlertKind, String)>,
    pub activity: Activity,
    pub remotes: Vec<RemoteView>,
}

pub(crate) fn frontend_label(frontend: &str) -> String {
    match frontend {
        "fuse" => "FUSE".into(),
        "winfsp" => "WinFsp".into(),
        "dav" => "WebDAV".into(),
        other => other.to_string(),
    }
}

pub(crate) fn remote_view(remote: &RemoteTraffic, now: u64) -> RemoteView {
    let activity = activity(remote);
    RemoteView {
        remote: remote.remote.clone(),
        backend: remote.backend.clone(),
        activity,
        upload_rate: format::rate(remote.upload_rate_10s),
        download_rate: format::rate(remote.download_rate_10s),
        totals: trf(
            "Sent {sent} · acknowledged {acked} · verified {verified} · received {received}",
            &[
                ("sent", &format::bytes(remote.sent_bytes)),
                ("acked", &format::bytes(remote.acked_bytes)),
                ("verified", &format::bytes(remote.verified_bytes)),
                ("received", &format::bytes(remote.received_bytes)),
            ],
        ),
        transfers: trf(
            "{up} uploads and {down} downloads running",
            &[
                ("up", &remote.active_uploads),
                ("down", &remote.active_downloads),
            ],
        ),
        ops: trf(
            "{ok} OK · {failed} failed · {retries} retries",
            &[
                ("ok", &remote.ok_ops),
                ("failed", &remote.failed_ops),
                ("retries", &remote.retries),
            ],
        ),
        last_ok: match remote.last_ok_unix {
            Some(at) => trf("Last OK {age}", &[("age", &relative_age_at(now, at))]),
            None => tr("No successful operation yet").into(),
        },
        last_error: remote
            .last_error
            .as_ref()
            .map(|error| match remote.last_error_unix {
                Some(at) => format!("{error} ({})", relative_age_at(now, at)),
                None => error.clone(),
            }),
    }
}

pub(crate) fn mount_view(entry: &MountEntry, status: Option<&NetStatus>, now: u64) -> MountView {
    MountView {
        pool: entry.pool.clone(),
        mountpoint: entry.mountpoint.clone(),
        frontend: frontend_label(&entry.frontend),
        live: status.map(|status| live_view(status, now)),
    }
}

fn live_view(status: &NetStatus, now: u64) -> LiveView {
    let remotes: Vec<RemoteView> = status
        .remotes
        .iter()
        .map(|remote| remote_view(remote, now))
        .collect();
    let activity = [Activity::Uploading, Activity::Downloading]
        .into_iter()
        .find(|wanted| remotes.iter().any(|r| r.activity == *wanted))
        .unwrap_or(Activity::Idle);
    let queue = &status.queue;
    LiveView {
        uptime: trf(
            "Up {duration}",
            &[("duration", &format::duration(status.uptime_seconds))],
        ),
        last_sync: match status.last_sync_unix {
            Some(at) => trf("Last sync {age}", &[("age", &relative_age_at(now, at))]),
            None => tr("Not synced yet").into(),
        },
        queue: if queue.pending_files == 0 {
            tr("Nothing waiting to upload").into()
        } else {
            trf(
                "{files} files, {bytes} waiting",
                &[
                    ("files", &queue.pending_files),
                    ("bytes", &format::bytes(queue.pending_bytes)),
                ],
            )
        },
        queue_oldest: queue
            .oldest_pending_unix
            .filter(|_| queue.pending_files > 0)
            .map(|at| {
                trf(
                    "oldest since {age}",
                    &[("age", &format::duration(now.saturating_sub(at)))],
                )
            }),
        queue_waiting: queue.pending_files > 0,
        alerts: status
            .alerts
            .iter()
            .map(|alert| (alert.kind, alert_text(alert)))
            .collect(),
        activity,
        remotes,
    }
}
