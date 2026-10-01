//! Portable account-limit settings (`account_limits.json`): per-account
//! overrides of the daily upload budget, bandwidth, request rate and
//! inactivity warning, plus the global bandwidth timetable. Defaults come
//! from the backend type, so most accounts need no entry at all.
use super::bandwidth::Timetable;
use crate::prelude::*;

pub(crate) const LIMITS_VERSION: u32 = 1;
/// Google Drive: 750 GB uploaded (and copied) per user per 24 h.
pub(crate) const DRIVE_DAILY_UPLOAD: u64 = 750_000_000_000;
/// Google deletes personal accounts inactive for 2 years; warn at 18 months.
pub(crate) const DRIVE_INACTIVITY_WARN_DAYS: u32 = 548;
/// Microsoft may close inactive accounts; warn at 9 months.
pub(crate) const ONEDRIVE_INACTIVITY_WARN_DAYS: u32 = 274;
/// Default interval of the automatic keep-alive from a mount.
pub(crate) const DEFAULT_KEEPALIVE_DAYS: u32 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DailyUpload {
    /// No daily budget, also for backends that have a default.
    Unlimited,
    Bytes(u64),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_upload: Option<DailyUpload>,
    /// Timetable or rate in `--bwlimit` syntax for this account only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bwlimit: Option<String>,
    /// rclone `--tpslimit` for calls to this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpslimit: Option<f64>,
    /// `0` = no inactivity warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactivity_warn_days: Option<u32>,
}

impl AccountLimits {
    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn default_keepalive_days() -> u32 {
    DEFAULT_KEEPALIVE_DAYS
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LimitsStore {
    pub version: u32,
    /// Global timetable in `--bwlimit` syntax; `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<String>,
    /// A mount keeps its accounts alive when idle this long; `0` = never.
    #[serde(default = "default_keepalive_days")]
    pub keepalive_days: u32,
    /// Keyed by account (bottom remote name, no colon).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, AccountLimits>,
}

impl Default for LimitsStore {
    fn default() -> Self {
        Self {
            version: LIMITS_VERSION,
            bandwidth: None,
            keepalive_days: DEFAULT_KEEPALIVE_DAYS,
            accounts: BTreeMap::new(),
        }
    }
}

impl LimitsStore {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != LIMITS_VERSION {
            bail!("unsupported account limits version {}", self.version);
        }
        if let Some(text) = &self.bandwidth {
            Timetable::parse(text).context("invalid bandwidth timetable")?;
        }
        for (name, limits) in &self.accounts {
            if name.is_empty() || name.contains([':', '/', '\\']) {
                bail!("invalid account name {name:?}");
            }
            if let Some(text) = &limits.bwlimit {
                Timetable::parse(text).with_context(|| format!("invalid bwlimit for {name}"))?;
            }
            if let Some(tps) = limits.tpslimit {
                if !tps.is_finite() || tps <= 0.0 {
                    bail!("tpslimit for {name} must be a positive number");
                }
            }
            if limits.daily_upload == Some(DailyUpload::Bytes(0)) {
                bail!("daily upload budget for {name} must be positive (or unlimited)");
            }
        }
        Ok(())
    }
    pub(crate) fn account(&self, name: &str) -> Option<&AccountLimits> {
        self.accounts.get(name.trim_end_matches(':'))
    }
    /// Effective limits of `account` with backend type `kind`.
    pub(crate) fn effective(&self, account: &str, kind: &str) -> Effective {
        let own = self.account(account).cloned().unwrap_or_default();
        let daily_default = default_daily_upload(kind);
        let warn_default = default_inactivity_days(kind);
        Effective {
            daily_upload: match own.daily_upload {
                Some(DailyUpload::Unlimited) => None,
                Some(DailyUpload::Bytes(bytes)) => Some(bytes),
                None => daily_default,
            },
            daily_upload_is_default: own.daily_upload.is_none(),
            bwlimit: own.bwlimit.clone(),
            tpslimit: own.tpslimit,
            inactivity_warn_days: match own.inactivity_warn_days {
                Some(0) => None,
                Some(days) => Some(days),
                None => warn_default,
            },
            inactivity_is_default: own.inactivity_warn_days.is_none(),
        }
    }
}

/// What applies to one account after defaults.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Effective {
    pub daily_upload: Option<u64>,
    pub daily_upload_is_default: bool,
    pub bwlimit: Option<String>,
    pub tpslimit: Option<f64>,
    pub inactivity_warn_days: Option<u32>,
    pub inactivity_is_default: bool,
}

/// Daily upload budget of a backend type, when the provider documents one.
pub(crate) fn default_daily_upload(kind: &str) -> Option<u64> {
    match kind.to_ascii_lowercase().as_str() {
        "drive" => Some(DRIVE_DAILY_UPLOAD),
        _ => None,
    }
}

/// Inactivity warning threshold of a backend type.
pub(crate) fn default_inactivity_days(kind: &str) -> Option<u32> {
    match kind.to_ascii_lowercase().as_str() {
        "drive" => Some(DRIVE_INACTIVITY_WARN_DAYS),
        "onedrive" => Some(ONEDRIVE_INACTIVITY_WARN_DAYS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_backend_and_overrides_win() {
        let mut store = LimitsStore::default();
        let drive = store.effective("gd", "drive");
        assert_eq!(drive.daily_upload, Some(DRIVE_DAILY_UPLOAD));
        assert!(drive.daily_upload_is_default);
        assert_eq!(drive.inactivity_warn_days, Some(548));
        assert_eq!(
            store.effective("od", "onedrive").inactivity_warn_days,
            Some(274)
        );
        let other = store.effective("db", "dropbox");
        assert_eq!(
            (other.daily_upload, other.inactivity_warn_days),
            (None, None)
        );

        store.accounts.insert(
            "gd".into(),
            AccountLimits {
                daily_upload: Some(DailyUpload::Unlimited),
                inactivity_warn_days: Some(0),
                tpslimit: Some(4.0),
                ..Default::default()
            },
        );
        store.accounts.insert(
            "db".into(),
            AccountLimits {
                daily_upload: Some(DailyUpload::Bytes(5)),
                ..Default::default()
            },
        );
        let drive = store.effective("gd:", "drive");
        assert_eq!(
            (drive.daily_upload, drive.inactivity_warn_days),
            (None, None)
        );
        assert_eq!(drive.tpslimit, Some(4.0));
        assert!(!drive.daily_upload_is_default);
        assert_eq!(store.effective("db", "dropbox").daily_upload, Some(5));
        store.validate().unwrap();
    }

    #[test]
    fn validation_rejects_bad_values_and_json_round_trips() {
        let store = LimitsStore {
            bandwidth: Some("08:00,1M 18:00,off".into()),
            ..Default::default()
        };
        store.validate().unwrap();
        let text = serde_json::to_string(&store).unwrap();
        assert_eq!(serde_json::from_str::<LimitsStore>(&text).unwrap(), store);
        // Older/minimal files get the defaults.
        let minimal: LimitsStore = serde_json::from_str(r#"{"version":1}"#).unwrap();
        assert_eq!(minimal, LimitsStore::default());
        for bad in [
            LimitsStore {
                bandwidth: Some("fast".into()),
                ..Default::default()
            },
            LimitsStore {
                accounts: BTreeMap::from([(
                    "a".into(),
                    AccountLimits {
                        tpslimit: Some(0.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
            LimitsStore {
                accounts: BTreeMap::from([("a:".into(), AccountLimits::default())]),
                ..Default::default()
            },
            LimitsStore {
                accounts: BTreeMap::from([(
                    "a".into(),
                    AccountLimits {
                        daily_upload: Some(DailyUpload::Bytes(0)),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
