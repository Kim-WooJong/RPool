//! Process-wide view of the account limits for the rclone layer: cached
//! settings (re-read every 30 s, so a CLI or GUI change reaches running
//! mounts), the shared upload ledger, the in-process list of paused remotes
//! for monitoring, and throttled activity records.
//!
//! Unit tests never touch the user's config: settings are defaults and the
//! ledger is absent under `cfg(test)`; the decisions themselves are the
//! `*_with` functions, tested with explicit ledgers.
use super::bandwidth::{Rate, Timetable};
use super::budget::{self, Admission, PauseReason};
use super::ledger::Ledger;
use super::limits::LimitsStore;
use crate::storage::error::StorageError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SETTINGS_REFRESH: Duration = Duration::from_secs(30);
/// Seconds between two persisted activity records of one remote.
const ACTIVITY_PERSIST: u64 = 600;
/// After the provider reported its upload limit, wait this long before the
/// next upload attempt to that account.
pub(crate) const PROVIDER_HOLD_SECONDS: u64 = 3600;

pub(crate) struct Settings {
    pub store: LimitsStore,
    global: Option<Timetable>,
    accounts: HashMap<String, Timetable>,
}

impl Settings {
    pub(crate) fn from_store(store: LimitsStore) -> Self {
        let global = store
            .bandwidth
            .as_deref()
            .and_then(|text| Timetable::parse(text).ok());
        let accounts = store
            .accounts
            .iter()
            .filter_map(|(name, limits)| {
                let table = Timetable::parse(limits.bwlimit.as_deref()?).ok()?;
                Some((name.clone(), table))
            })
            .collect();
        Self {
            store,
            global,
            accounts,
        }
    }
    /// Whether any account has its own bandwidth or request-rate limit.
    pub(crate) fn has_account_overrides(&self) -> bool {
        self.store
            .accounts
            .values()
            .any(|limits| limits.bwlimit.is_some() || limits.tpslimit.is_some())
    }
    pub(crate) fn global_rate(&self, now: u64, offset: i64) -> (Rate, Option<u64>) {
        self.global
            .as_ref()
            .map_or((Rate::OFF, None), |table| table.at_unix(now, offset))
    }
    pub(crate) fn account_rate(&self, account: &str, now: u64, offset: i64) -> Rate {
        self.accounts
            .get(account)
            .map_or(Rate::OFF, |table| table.at_unix(now, offset).0)
    }
    /// This account's own cap on simultaneous uploads, if set.
    pub(crate) fn max_uploads(&self, account: &str) -> Option<usize> {
        self.store
            .account(account)
            .and_then(|limits| limits.max_uploads)
            .map(|n| n as usize)
    }
    /// The cap that applies to `account`: its own value, else the global
    /// default (not for Dropbox, which rejects concurrent writes), else
    /// `None` for the built-in backend default.
    pub(crate) fn upload_cap(&self, account: &str, dropbox: bool) -> Option<usize> {
        self.max_uploads(account).or_else(|| {
            self.store
                .default_max_uploads
                .filter(|_| !dropbox)
                .map(|n| n as usize)
        })
    }
    pub(crate) fn tpslimit(&self, account: &str) -> Option<f64> {
        self.store
            .account(account)
            .and_then(|limits| limits.tpslimit)
    }
}

/// Current settings of this process.
pub(crate) fn settings() -> Arc<Settings> {
    static CACHE: Mutex<Option<(Instant, Arc<Settings>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, settings)) = cache.as_ref() {
        if at.elapsed() < SETTINGS_REFRESH {
            return settings.clone();
        }
    }
    let store = if cfg!(test) {
        LimitsStore::default()
    } else {
        // An unreadable file must not stop transfers: no limits apply then.
        super::store::load_limits().unwrap_or_default()
    };
    let settings = Arc::new(Settings::from_store(store));
    *cache = Some((Instant::now(), settings.clone()));
    settings
}

/// Local UTC offset for timetables (UTC when unknown).
pub(crate) fn offset() -> i64 {
    crate::utils::local_offset_seconds().unwrap_or(0)
}

/// The shared ledger of this computer; `None` in unit tests or when the
/// config directory is unknown.
pub(crate) fn ledger() -> Option<Ledger> {
    static LEDGER: OnceLock<Option<Ledger>> = OnceLock::new();
    LEDGER
        .get_or_init(|| {
            if cfg!(test) {
                return None;
            }
            crate::config::account_usage_path().ok().map(Ledger::at)
        })
        .clone()
}

fn report_once(error: &anyhow::Error) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "Account usage ledger unavailable; daily upload budgets are not enforced: {error:#}"
        );
    }
}

/// One paused remote, for monitoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pause {
    pub account: String,
    /// When this process first saw the pause.
    pub since: u64,
    pub until: u64,
    pub reason: PauseReason,
}

fn paused() -> &'static Mutex<HashMap<String, Pause>> {
    static PAUSED: OnceLock<Mutex<HashMap<String, Pause>>> = OnceLock::new();
    PAUSED.get_or_init(Default::default)
}

/// Remotes (rclone remote names) whose uploads wait now, with their pause.
pub(crate) fn paused_remotes(now: u64) -> Vec<(String, Pause)> {
    let mut table = paused().lock().unwrap_or_else(|p| p.into_inner());
    table.retain(|_, pause| pause.until > now);
    let mut out: Vec<_> = table.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn note_pause(remote: &str, pause: Option<Pause>) {
    let mut table = paused().lock().unwrap_or_else(|p| p.into_inner());
    match pause {
        Some(mut pause) => {
            if let Some(old) = table.get(remote).filter(|old| old.until > pause.since) {
                pause.since = pause.since.min(old.since);
            }
            table.insert(remote.to_owned(), pause);
        }
        None => {
            table.remove(remote);
        }
    }
}

/// The error a paused upload returns: retriable, no `retry_after` (a pause
/// can last hours; callers keep their work queued instead of sleeping).
pub(crate) fn paused_error(
    account: &str,
    until: u64,
    reason: PauseReason,
    now: u64,
) -> StorageError {
    let wait = budget::wait_text(until.saturating_sub(now));
    let detail = match reason {
        PauseReason::Budget => {
            format!("{account}: daily upload limit reached; uploads resume in {wait}")
        }
        PauseReason::Provider => {
            format!("{account}: the provider reported its upload limit; uploads retry in {wait}")
        }
    };
    StorageError::RateLimited {
        retry_after: None,
        detail,
    }
}

/// Whether an upload to `account` (backend `kind`) may start at `now`.
pub(crate) fn admit_with(
    ledger: &Ledger,
    store: &LimitsStore,
    account: &str,
    kind: &str,
    now: u64,
) -> Result<Admission, anyhow::Error> {
    let limit = store.effective(account, kind).daily_upload;
    let data = ledger.load()?;
    Ok(budget::evaluate(data.accounts.get(account), limit, now).admission)
}

/// Admission for an upload of `remote` (the address's remote name) to
/// `account`; a refusal is recorded for monitoring.
pub(crate) fn admit(remote: &str, account: &str, kind: &str) -> Result<(), StorageError> {
    let Some(ledger) = ledger() else {
        return Ok(());
    };
    let now = crate::utils::now_unix();
    match admit_with(&ledger, &settings().store, account, kind, now) {
        Ok(Admission::Open) => {
            note_pause(remote, None);
            Ok(())
        }
        Ok(Admission::Paused { until, reason }) => {
            note_pause(
                remote,
                Some(Pause {
                    account: account.to_owned(),
                    since: now,
                    until,
                    reason,
                }),
            );
            Err(paused_error(account, until, reason, now))
        }
        Err(error) => {
            report_once(&error);
            Ok(())
        }
    }
}

/// Counts `bytes` sent to `account` now.
pub(crate) fn record_upload(account: &str, bytes: u64) {
    if bytes == 0 {
        return;
    }
    if let Some(ledger) = ledger() {
        let now = crate::utils::now_unix();
        if let Err(error) = ledger.update(|data| data.account(account).add(now, bytes)) {
            report_once(&error);
        }
    }
}

/// Records that the provider refused uploads for its limit at `now`.
pub(crate) fn record_provider_limit_with(
    ledger: &Ledger,
    account: &str,
    now: u64,
) -> anyhow::Result<u64> {
    let until = now + PROVIDER_HOLD_SECONDS;
    ledger.update(|data| {
        let usage = data.account(account);
        usage.provider_pause_until = Some(usage.provider_pause_until.unwrap_or(0).max(until));
    })?;
    Ok(until)
}

pub(crate) fn record_provider_limit(remote: &str, account: &str) {
    let now = crate::utils::now_unix();
    let until = match ledger() {
        Some(ledger) => record_provider_limit_with(&ledger, account, now).unwrap_or_else(|error| {
            report_once(&error);
            now + PROVIDER_HOLD_SECONDS
        }),
        None => return,
    };
    note_pause(
        remote,
        Some(Pause {
            account: account.to_owned(),
            since: now,
            until,
            reason: PauseReason::Provider,
        }),
    );
}

/// A successful operation on remote `name`; persisted at most every 10 min
/// per remote, so the hot path stays in memory.
pub(crate) fn note_activity(name: &str) {
    let Some(ledger) = ledger() else {
        return;
    };
    static SEEN: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let now = crate::utils::now_unix();
    {
        let mut seen = SEEN
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match seen.get(name) {
            Some(last) if now.saturating_sub(*last) < ACTIVITY_PERSIST => return,
            _ => {
                seen.insert(name.to_owned(), now);
            }
        }
    }
    if let Err(error) = ledger.update(|data| data.note_activity(name, now)) {
        report_once(&error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::account::limits::{AccountLimits, DailyUpload};

    #[test]
    fn budget_exhaustion_waits_and_resumes_with_a_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path().join("usage.json"));
        let mut store = LimitsStore::default();
        store.accounts.insert(
            "gd".into(),
            AccountLimits {
                daily_upload: Some(DailyUpload::Bytes(100)),
                ..Default::default()
            },
        );
        let now = 10_000 * 3600;
        assert_eq!(
            admit_with(&ledger, &store, "gd", "drive", now).unwrap(),
            Admission::Open
        );
        ledger.update(|d| d.account("gd").add(now, 100)).unwrap();
        let until = crate::storage::account::ledger::AccountUsage::expires_at(10_000);
        let paused = admit_with(&ledger, &store, "gd", "drive", now + 60).unwrap();
        assert_eq!(
            paused,
            Admission::Paused {
                until,
                reason: PauseReason::Budget
            }
        );
        let error = paused_error("gd", until, PauseReason::Budget, now + 60);
        assert!(error.is_retriable() && error.retry_after().is_none());
        assert!(
            error
                .to_string()
                .contains("gd: daily upload limit reached; uploads resume in 24 h 59 min"),
            "{error}"
        );
        assert_eq!(
            admit_with(&ledger, &store, "gd", "drive", until).unwrap(),
            Admission::Open
        );
        // Another account sharing the ledger is unaffected.
        assert_eq!(
            admit_with(&ledger, &store, "other", "drive", now).unwrap(),
            Admission::Open
        );
    }

    #[test]
    fn provider_limit_pauses_any_account_for_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path().join("usage.json"));
        let store = LimitsStore::default();
        let until = record_provider_limit_with(&ledger, "db", 1000).unwrap();
        assert_eq!(until, 1000 + PROVIDER_HOLD_SECONDS);
        assert_eq!(
            admit_with(&ledger, &store, "db", "dropbox", 1001).unwrap(),
            Admission::Paused {
                until,
                reason: PauseReason::Provider
            }
        );
        // A later, shorter report never shortens the pause.
        record_provider_limit_with(&ledger, "db", 500).unwrap();
        assert!(matches!(
            admit_with(&ledger, &store, "db", "dropbox", until - 1).unwrap(),
            Admission::Paused { until: u, .. } if u == until
        ));
        assert_eq!(
            admit_with(&ledger, &store, "db", "dropbox", until).unwrap(),
            Admission::Open
        );
        assert!(paused_error("db", until, PauseReason::Provider, 1000)
            .to_string()
            .contains("provider reported its upload limit"));
    }

    #[test]
    fn settings_resolve_rates_and_overrides() {
        let mut store = LimitsStore {
            bandwidth: Some("1M".into()),
            ..Default::default()
        };
        let settings = Settings::from_store(store.clone());
        assert_eq!(settings.global_rate(0, 0).0.up, Some(1 << 20));
        assert!(!settings.has_account_overrides());
        store.accounts.insert(
            "gd".into(),
            AccountLimits {
                bwlimit: Some("off:2M".into()),
                tpslimit: Some(2.5),
                ..Default::default()
            },
        );
        let settings = Settings::from_store(store);
        assert!(settings.has_account_overrides());
        assert_eq!(settings.account_rate("gd", 0, 0).down, Some(2 << 20));
        assert_eq!(settings.account_rate("x", 0, 0), Rate::OFF);
        assert_eq!(settings.tpslimit("gd"), Some(2.5));
    }
}
