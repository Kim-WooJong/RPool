//! What one mount card shows, computed from its registry entry and live
//! status at a given time (no egui, so it is unit tested).
use super::format;
use crate::gui::i18n::{relative_age_at, tr, trf};
use crate::monitor::model::{Alert, AlertKind, MountEntry, NetStatus, RemoteTraffic};

/// What an account (or a whole mount) is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    /// Sending data, or uploads are active.
    Uploading,
    /// Receiving data, or downloads are active.
    Downloading,
    /// No transfer.
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

/// Translated activity badge text.
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
        (AlertKind::MetadataGrowing, _) => tr(
            "Drive metadata is growing without a checkpoint; new PCs may not open it. Compact it on the Pools page.",
        )
        .into(),
        (AlertKind::UploadLimit, Some(remote)) => trf(
            "{remote}: upload limit reached — uploads wait and resume automatically",
            &[("remote", &remote)],
        ),
        (AlertKind::UploadLimit, None) => {
            tr("An account reached its upload limit — uploads wait and resume automatically").into()
        }
    }
}

/// Display texts of one account on a mount card, from `remote_view`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteView {
    /// Remote (account) name.
    pub remote: String,
    /// rclone backend type, when known.
    pub backend: Option<String>,
    /// Current activity of the account.
    pub activity: Activity,
    /// Upload rate text (10 s average).
    pub upload_rate: String,
    /// Download rate text (10 s average).
    pub download_rate: String,
    /// Sent / acknowledged / verified / received byte totals.
    pub totals: String,
    /// Running uploads and downloads.
    pub transfers: String,
    /// OK, failed and retried operation counts.
    pub ops: String,
    /// Age of the last successful operation.
    pub last_ok: String,
    /// Error line and its age.
    pub last_error: Option<String>,
    /// Uploads wait for the account's upload limit: the alert's detail
    /// (English, with the resume time) for the badge's hover text.
    pub upload_limit: Option<String>,
}

/// Everything one mount card shows, from `mount_view`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MountView {
    /// Pool name (card title).
    pub pool: String,
    /// Mount point (card subtitle).
    pub mountpoint: String,
    /// Mount frontend label (FUSE, WinFsp, WebDAV).
    pub frontend: String,
    /// `None` while no fresh status was read.
    pub live: Option<LiveView>,
}

/// Live part of a mount card, built from the latest status.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveView {
    /// "Up …" uptime text.
    pub uptime: String,
    /// Time since the last metadata sync.
    pub last_sync: String,
    /// Upload queue text (files and bytes waiting).
    pub queue: String,
    /// Oldest waiting change, when any.
    pub queue_oldest: Option<String>,
    /// Whether anything waits to upload (highlights the queue line).
    pub queue_waiting: bool,
    /// Alert kinds with their translated banner texts.
    pub alerts: Vec<(AlertKind, String)>,
    /// Overall activity: uploading if any account uploads, else downloading, else idle.
    pub activity: Activity,
    /// One entry per account, in status order.
    pub remotes: Vec<RemoteView>,
}

/// Display name of a mount frontend id (`fuse`, `winfsp`, `dav`); unknown ids unchanged.
pub(crate) fn frontend_label(frontend: &str) -> String {
    match frontend {
        "fuse" => "FUSE".into(),
        "winfsp" => "WinFsp".into(),
        "dav" => "WebDAV".into(),
        other => other.to_string(),
    }
}

/// Display texts of one account's traffic at time `now` (no upload-limit
/// detail; `live_view` adds it).
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
        upload_limit: None,
    }
}

/// The card of a mount: registry fields plus the live part when a status is
/// present. Called by the Monitoring page for every mount each frame.
pub(crate) fn mount_view(entry: &MountEntry, status: Option<&NetStatus>, now: u64) -> MountView {
    MountView {
        pool: entry.pool.clone(),
        mountpoint: entry.mountpoint.clone(),
        frontend: frontend_label(&entry.frontend),
        live: status.map(|status| live_view(status, now)),
    }
}

/// Live part of a card: per-account views with upload-limit details, overall
/// activity, uptime, sync, queue and translated alerts.
fn live_view(status: &NetStatus, now: u64) -> LiveView {
    let remotes: Vec<RemoteView> = status
        .remotes
        .iter()
        .map(|remote| {
            let mut view = remote_view(remote, now);
            view.upload_limit = status
                .alerts
                .iter()
                .find(|a| {
                    a.kind == AlertKind::UploadLimit && a.remote.as_deref() == Some(&remote.remote)
                })
                .map(|a| a.message.clone());
            view
        })
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
