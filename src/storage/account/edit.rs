//! Applying one account's limit edits (from the CLI or the GUI editor) to the
//! settings, so both front ends interpret every value the same way.
use super::bandwidth::Timetable;
use super::limits::{DailyUpload, LimitsStore};
use crate::prelude::*;

const GIB: f64 = (1u64 << 30) as f64;

/// Daily budget edit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DailyEdit {
    Gib(f64),
    Unlimited,
    BackendDefault,
}

/// Changes for one account; `None` fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LimitEdit {
    pub daily: Option<DailyEdit>,
    /// `--bwlimit` syntax; `off` (or empty) removes the account's own limit.
    pub bwlimit: Option<String>,
    /// `0` removes the request-rate limit.
    pub tpslimit: Option<f64>,
    /// `0` = never warn.
    pub inactivity_warn_days: Option<u32>,
}

pub(crate) fn apply(store: &mut LimitsStore, account: &str, edit: &LimitEdit) -> Result<()> {
    let account = account.trim().trim_end_matches(':');
    if account.is_empty() || account.contains(['/', '\\', ':']) {
        bail!("invalid account name {account:?}");
    }
    let mut limits = store.accounts.get(account).cloned().unwrap_or_default();
    match edit.daily {
        Some(DailyEdit::Gib(gib)) => {
            if !gib.is_finite() || gib <= 0.0 || gib * GIB >= u64::MAX as f64 {
                bail!("daily upload budget must be a positive number of GiB");
            }
            limits.daily_upload = Some(DailyUpload::Bytes((gib * GIB).round().max(1.0) as u64));
        }
        Some(DailyEdit::Unlimited) => limits.daily_upload = Some(DailyUpload::Unlimited),
        Some(DailyEdit::BackendDefault) => limits.daily_upload = None,
        None => {}
    }
    if let Some(text) = &edit.bwlimit {
        let text = text.trim();
        limits.bwlimit = if text.is_empty() || text.eq_ignore_ascii_case("off") {
            None
        } else {
            Timetable::parse(text)?;
            Some(text.to_owned())
        };
    }
    if let Some(tps) = edit.tpslimit {
        if !tps.is_finite() || tps < 0.0 {
            bail!("tpslimit must be zero or a positive number");
        }
        limits.tpslimit = (tps > 0.0).then_some(tps);
    }
    if let Some(days) = edit.inactivity_warn_days {
        limits.inactivity_warn_days = Some(days);
    }
    if limits.is_empty() {
        store.accounts.remove(account);
    } else {
        store.accounts.insert(account.to_owned(), limits);
    }
    store.validate()
}

/// Sets (or with `off`/empty, removes) the global bandwidth timetable.
pub(crate) fn set_bandwidth(store: &mut LimitsStore, text: &str) -> Result<()> {
    let text = text.trim();
    store.bandwidth = if text.is_empty() || text.eq_ignore_ascii_case("off") {
        None
    } else {
        Timetable::parse(text)?;
        Some(text.to_owned())
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_set_and_clear_each_field() {
        let mut store = LimitsStore::default();
        let edit = LimitEdit {
            daily: Some(DailyEdit::Gib(1.5)),
            bwlimit: Some("10M:off".into()),
            tpslimit: Some(4.0),
            inactivity_warn_days: Some(0),
        };
        apply(&mut store, "gd:", &edit).unwrap();
        let gd = &store.accounts["gd"];
        assert_eq!(gd.daily_upload, Some(DailyUpload::Bytes(3 << 29)));
        assert_eq!(gd.bwlimit.as_deref(), Some("10M:off"));
        assert_eq!((gd.tpslimit, gd.inactivity_warn_days), (Some(4.0), Some(0)));
        apply(
            &mut store,
            "gd",
            &LimitEdit {
                daily: Some(DailyEdit::Unlimited),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            store.accounts["gd"].daily_upload,
            Some(DailyUpload::Unlimited)
        );
        assert_eq!(store.accounts["gd"].tpslimit, Some(4.0)); // untouched
                                                              // Clearing everything removes the entry.
        apply(
            &mut store,
            "gd",
            &LimitEdit {
                daily: Some(DailyEdit::BackendDefault),
                bwlimit: Some("off".into()),
                tpslimit: Some(0.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(store.accounts["gd"].inactivity_warn_days, Some(0));
        store.accounts.get_mut("gd").unwrap().inactivity_warn_days = None;
        apply(&mut store, "gd", &LimitEdit::default()).unwrap();
        assert!(store.accounts.is_empty());
        for bad in [
            LimitEdit {
                daily: Some(DailyEdit::Gib(0.0)),
                ..Default::default()
            },
            LimitEdit {
                bwlimit: Some("fast".into()),
                ..Default::default()
            },
            LimitEdit {
                tpslimit: Some(-1.0),
                ..Default::default()
            },
        ] {
            assert!(apply(&mut store, "gd", &bad).is_err(), "{bad:?}");
        }
        assert!(apply(&mut store, "a/b", &LimitEdit::default()).is_err());
        set_bandwidth(&mut store, "08:00,1M 18:00,off").unwrap();
        assert!(store.bandwidth.is_some());
        assert!(set_bandwidth(&mut store, "nope").is_err());
        set_bandwidth(&mut store, "off").unwrap();
        assert!(store.bandwidth.is_none());
    }
}
