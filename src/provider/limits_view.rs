//! One status row per cloud account: daily upload budget, pause, last
//! activity and inactivity warning, and the configured overrides. Shared by
//! `rpool provider limits show` and the GUI provider cards. Pure: the caller
//! passes the accounts, ledger, settings and clock.
use crate::prelude::*;
use crate::storage::account::budget::{self, Admission, PauseReason};
use crate::storage::account::inactivity::{self, Level};
use crate::storage::account::ledger::LedgerData;
use crate::storage::account::limits::LimitsStore;
use crate::storage::admin::RemoteCatalog;

/// One account and the crypt remotes stored in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountRef {
    pub name: String,
    pub kind: String,
    /// Crypt remote names (with or without colon).
    pub crypts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AccountStatus {
    pub account: String,
    pub kind: String,
    pub crypts: Vec<String>,
    /// Effective daily upload budget in bytes; `None` = unlimited.
    pub daily_upload_limit: Option<u64>,
    pub daily_upload_is_default: bool,
    /// Bytes this computer uploaded in the rolling 24 h window.
    pub uploaded_24h: u64,
    /// When the oldest counted bytes leave the window.
    pub next_release_unix: Option<u64>,
    pub paused_until_unix: Option<u64>,
    /// `budget` or `provider`.
    pub pause_reason: Option<&'static str>,
    /// Last successful operation from this computer (account or its crypts).
    pub last_activity_unix: Option<u64>,
    pub inactive_days: Option<u64>,
    pub inactivity_warn_days: Option<u32>,
    pub inactivity_is_default: bool,
    /// `untracked`, `unknown`, `fine`, `near` or `exceeded`.
    pub inactivity: &'static str,
    pub last_keepalive_unix: Option<u64>,
    pub bwlimit: Option<String>,
    pub tpslimit: Option<f64>,
}

impl AccountStatus {
    pub(crate) fn inactivity_level(&self) -> Level {
        match self.inactivity {
            "unknown" => Level::Unknown,
            "fine" => Level::Fine,
            "near" => Level::Near,
            "exceeded" => Level::Exceeded,
            _ => Level::Untracked,
        }
    }
    /// Used share of the daily budget (0..=1), when there is one.
    pub(crate) fn budget_ratio(&self) -> Option<f32> {
        let limit = self.daily_upload_limit.filter(|l| *l > 0)?;
        Some((self.uploaded_24h as f64 / limit as f64).clamp(0.0, 1.0) as f32)
    }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Untracked => "untracked",
        Level::Unknown => "unknown",
        Level::Fine => "fine",
        Level::Near => "near",
        Level::Exceeded => "exceeded",
    }
}

pub(crate) fn build(
    accounts: &[AccountRef],
    ledger: &LedgerData,
    store: &LimitsStore,
    now: u64,
) -> Vec<AccountStatus> {
    accounts
        .iter()
        .map(|account| {
            let effective = store.effective(&account.name, &account.kind);
            let usage = ledger.accounts.get(&account.name);
            let view = budget::evaluate(usage, effective.daily_upload, now);
            let (paused_until_unix, pause_reason) = match view.admission {
                Admission::Open => (None, None),
                Admission::Paused { until, reason } => (
                    Some(until),
                    Some(match reason {
                        PauseReason::Budget => "budget",
                        PauseReason::Provider => "provider",
                    }),
                ),
            };
            let names = std::iter::once(account.name.as_str())
                .chain(account.crypts.iter().map(String::as_str));
            let last_activity = ledger.last_activity(names);
            let idle = inactivity::evaluate(last_activity, effective.inactivity_warn_days, now);
            AccountStatus {
                account: account.name.clone(),
                kind: account.kind.clone(),
                crypts: account.crypts.clone(),
                daily_upload_limit: effective.daily_upload,
                daily_upload_is_default: effective.daily_upload_is_default,
                uploaded_24h: view.used,
                next_release_unix: view.next_release,
                paused_until_unix,
                pause_reason,
                last_activity_unix: last_activity,
                inactive_days: idle.age_days,
                inactivity_warn_days: idle.warn_days,
                inactivity_is_default: effective.inactivity_is_default,
                inactivity: level_name(idle.level),
                last_keepalive_unix: usage.and_then(|u| u.last_keepalive),
                bwlimit: effective.bwlimit,
                tpslimit: effective.tpslimit,
            }
        })
        .collect()
}

/// Every backing account of the rclone config with its crypt remotes.
pub(crate) fn accounts_from_catalog(catalog: &RemoteCatalog) -> Vec<AccountRef> {
    let crypts = catalog.crypt_remotes();
    catalog
        .backing_remotes()
        .into_iter()
        .map(|name| AccountRef {
            kind: catalog.backend_kind(&name).unwrap_or("").to_owned(),
            crypts: crypts
                .iter()
                .filter(|crypt| catalog.placement_target(crypt).is_ok_and(|t| t == name))
                .cloned()
                .collect(),
            name,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::account::limits::{AccountLimits, DailyUpload};

    #[test]
    fn rows_join_budget_pause_activity_and_overrides() {
        let now = 1_000 * 86_400;
        let mut ledger = LedgerData::default();
        ledger.account("gd").add(now - 3600, 700_000_000_000);
        ledger.account("gd").add(now, 60_000_000_000);
        ledger.note_activity("gd_crypt", now - 450 * 86_400);
        ledger.account("od").provider_pause_until = Some(now + 600);
        let mut store = LimitsStore::default();
        store.accounts.insert(
            "od".into(),
            AccountLimits {
                tpslimit: Some(3.0),
                inactivity_warn_days: Some(0),
                daily_upload: Some(DailyUpload::Bytes(1 << 30)),
                ..Default::default()
            },
        );
        let accounts = [
            AccountRef {
                name: "gd".into(),
                kind: "drive".into(),
                crypts: vec!["gd_crypt:".into()],
            },
            AccountRef {
                name: "od".into(),
                kind: "onedrive".into(),
                crypts: vec![],
            },
        ];
        let rows = build(&accounts, &ledger, &store, now);
        let gd = &rows[0];
        assert_eq!(gd.uploaded_24h, 760_000_000_000);
        assert_eq!(gd.daily_upload_limit, Some(750_000_000_000));
        assert_eq!(gd.pause_reason, Some("budget"));
        assert!(gd.paused_until_unix.unwrap() > now);
        assert_eq!(gd.budget_ratio(), Some(1.0));
        assert_eq!((gd.inactivity, gd.inactive_days), ("near", Some(450)));
        let od = &rows[1];
        assert_eq!(
            (od.pause_reason, od.paused_until_unix),
            (Some("provider"), Some(now + 600))
        );
        assert_eq!((od.inactivity, od.tpslimit), ("untracked", Some(3.0)));
        assert_eq!(od.last_activity_unix, None);
        assert!(!od.daily_upload_is_default);
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(json[0]["pause_reason"], "budget");
    }

    #[test]
    fn catalog_accounts_list_their_crypts() {
        let catalog = RemoteCatalog::parse(&serde_json::json!({
            "gd": {"type": "drive"},
            "gd_crypt": {"type": "crypt", "remote": "gd:pool", "password": "x"},
            "db": {"type": "dropbox"}
        }))
        .unwrap();
        let accounts = accounts_from_catalog(&catalog);
        let gd = accounts.iter().find(|a| a.name == "gd").unwrap();
        assert_eq!(
            (gd.kind.as_str(), gd.crypts.clone()),
            ("drive", vec!["gd_crypt:".to_string()])
        );
        let db = accounts.iter().find(|a| a.name == "db").unwrap();
        assert!(db.crypts.is_empty());
    }
}
