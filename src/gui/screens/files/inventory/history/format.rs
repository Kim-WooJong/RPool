//! Short texts for times in the trash and version lists.

use super::clock::format_local;
use crate::gui::i18n::{relative_age_at, tr, trf};

const DAY: u64 = 86_400;

/// When a trashed entry leaves the trash: "in 3 days", "today", "expired",
/// "kept until deleted".
pub(crate) fn expiry(now: u64, expires: Option<u64>) -> String {
    let Some(expires) = expires else {
        return tr("Kept until deleted").into();
    };
    if expires <= now {
        return tr("Expired").into();
    }
    let left = expires - now;
    match left.div_ceil(DAY) {
        _ if left < DAY => tr("Expires today").into(),
        1 => tr("Expires in 1 day").into(),
        n => trf("Expires in {n} days", &[("n", &n)]),
    }
}

/// Whether the entry is past its expiry (removed at the next cleanup).
pub(crate) fn is_expired(now: u64, expires: Option<u64>) -> bool {
    expires.is_some_and(|expires| expires <= now)
}

/// Fewer than 3 days left: shown as a warning.
pub(crate) fn expires_soon(now: u64, expires: Option<u64>) -> bool {
    expires.is_some_and(|expires| expires <= now + 3 * DAY)
}

/// "3h ago" and the local time on hover; "unknown" without a time.
pub(crate) fn when(now: u64, time: Option<u64>, offset: i64) -> (String, String) {
    match time {
        Some(time) => (relative_age_at(now, time), format_local(time, offset)),
        None => (tr("unknown").into(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_texts() {
        let now = 1_000_000;
        assert_eq!(expiry(now, None), "Kept until deleted");
        assert_eq!(expiry(now, Some(now)), "Expired");
        assert_eq!(expiry(now, Some(now - 5)), "Expired");
        assert_eq!(expiry(now, Some(now + 60)), "Expires today");
        assert_eq!(expiry(now, Some(now + DAY)), "Expires in 1 day");
        assert_eq!(expiry(now, Some(now + DAY + 1)), "Expires in 2 days");
        assert_eq!(expiry(now, Some(now + 30 * DAY)), "Expires in 30 days");
        assert!(is_expired(now, Some(now)) && !is_expired(now, None));
        assert!(expires_soon(now, Some(now + 2 * DAY)));
        assert!(!expires_soon(now, Some(now + 4 * DAY)) && !expires_soon(now, None));
    }

    #[test]
    fn age_and_hover_time() {
        let (age, at) = when(1_727_740_800 + 7_200, Some(1_727_740_800), 0);
        assert_eq!((age.as_str(), at.as_str()), ("2h ago", "2024-10-01 00:00"));
        assert_eq!(when(5, None, 0), ("unknown".into(), String::new()));
    }
}
