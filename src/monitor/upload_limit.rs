//! `UploadLimit` alerts: pool remotes whose account is paused by its daily
//! upload budget or by the provider's own upload limit (uploads stay queued
//! and resume by themselves). The message carries the resume time.
use super::model::{Alert, AlertKind};
use crate::storage::account::budget::{wait_text, PauseReason};
use crate::storage::account::runtime::Pause;

/// Alerts for `remotes` (pool addresses) among `paused` (remote name, pause).
pub(crate) fn alerts(now: u64, remotes: &[String], paused: &[(String, Pause)]) -> Vec<Alert> {
    remotes
        .iter()
        .filter_map(|remote| {
            let name = crate::storage::rclone::remote_name(remote).ok()?;
            let (_, pause) = paused.iter().find(|(n, p)| n == name && p.until > now)?;
            let wait = wait_text(pause.until - now);
            let message = match pause.reason {
                PauseReason::Budget => format!(
                    "{remote}: account {} reached its daily upload limit; uploads resume in {wait}",
                    pause.account
                ),
                PauseReason::Provider => format!(
                    "{remote}: the provider reported the upload limit of account {}; uploads retry in {wait}",
                    pause.account
                ),
            };
            Some(Alert {
                kind: AlertKind::UploadLimit,
                remote: Some(remote.clone()),
                since_unix: pause.since,
                message,
            })
        })
        .collect()
}

/// Alerts of this process's paused remotes.
pub(crate) fn current(now: u64, remotes: &[String]) -> Vec<Alert> {
    alerts(
        now,
        remotes,
        &crate::storage::account::runtime::paused_remotes(now),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_pool_remotes_get_one_alert_until_they_resume() {
        let paused = vec![(
            "gd_crypt".to_string(),
            Pause {
                account: "gd".into(),
                since: 900,
                until: 1000 + 3 * 3600,
                reason: PauseReason::Budget,
            },
        )];
        let remotes = ["gd_crypt:pool".to_string(), "db_crypt:pool".into()];
        let alerts = alerts(1000, &remotes, &paused);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::UploadLimit);
        assert_eq!(alerts[0].remote.as_deref(), Some("gd_crypt:pool"));
        assert_eq!(alerts[0].since_unix, 900);
        assert!(
            alerts[0].message.contains("resume in 3 h 00 min"),
            "{}",
            alerts[0].message
        );
        assert!(super::alerts(1000 + 3 * 3600, &remotes, &paused).is_empty());
    }
}
