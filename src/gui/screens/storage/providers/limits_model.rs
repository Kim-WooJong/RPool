//! What a provider card shows about its account limits: the rolling 24 h
//! upload budget with its reset, a pause, and the last activity with the
//! inactivity warning. No egui, so it is unit tested.
use crate::gui::i18n::{relative_age_at, tr, trf};
use crate::presentation::format_bytes;
use crate::provider::limits_view::AccountStatus;
use crate::storage::account::inactivity::Level;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Muted,
    Warning,
    Danger,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BudgetLine {
    pub ratio: f32,
    /// Text on the bar, e.g. `12%`.
    pub bar: String,
    /// `Uploaded (24 h): 90 GiB of 698 GiB`.
    pub line: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardLimits {
    /// Only for accounts with a daily budget.
    pub budget: Option<BudgetLine>,
    /// Pause or window reset, with its tone.
    pub state: Option<(String, Tone)>,
    pub activity: String,
    pub activity_tone: Tone,
}

/// "3 h 05 min", "12 min", "under 1 min" in the current language.
pub(crate) fn wait(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    match minutes {
        0 => tr("under 1 min").into(),
        1..=59 => trf("{n} min", &[("n", &minutes)]),
        _ => trf(
            "{h} h {m} min",
            &[
                ("h", &(minutes / 60)),
                ("m", &format!("{:02}", minutes % 60)),
            ],
        ),
    }
}

pub(crate) fn card_limits(status: &AccountStatus, now: u64) -> CardLimits {
    let budget = status.budget_ratio().map(|ratio| BudgetLine {
        ratio,
        bar: format!("{:.0}%", ratio * 100.0),
        line: trf(
            "Uploaded (24 h): {used} of {limit}",
            &[
                ("used", &format_bytes(status.uploaded_24h)),
                (
                    "limit",
                    &format_bytes(status.daily_upload_limit.unwrap_or(0)),
                ),
            ],
        ),
    });
    let state = match (status.pause_reason, status.paused_until_unix) {
        (Some("provider"), Some(until)) => Some((
            trf(
                "Provider upload limit — uploads retry in {wait}",
                &[("wait", &wait(until.saturating_sub(now)))],
            ),
            Tone::Warning,
        )),
        (Some(_), Some(until)) => Some((
            trf(
                "Daily upload limit reached — uploads resume in {wait}",
                &[("wait", &wait(until.saturating_sub(now)))],
            ),
            Tone::Warning,
        )),
        _ => status
            .next_release_unix
            .filter(|_| budget.is_some() && status.uploaded_24h > 0)
            .map(|at| {
                (
                    trf(
                        "Oldest uploads leave the 24 h window in {wait}",
                        &[("wait", &wait(at.saturating_sub(now)))],
                    ),
                    Tone::Muted,
                )
            }),
    };
    let age = match status.last_activity_unix {
        Some(at) => trf("Last activity {age}", &[("age", &relative_age_at(now, at))]),
        None => tr("No activity from this computer yet").into(),
    };
    let (activity, activity_tone) = match (status.inactivity_level(), status.inactivity_warn_days) {
        (Level::Near, Some(days)) => (
            trf(
                "{age} · inactivity warning at {days} days",
                &[("age", &age), ("days", &days)],
            ),
            Tone::Warning,
        ),
        (Level::Exceeded, Some(days)) => (
            trf(
                "{age} · over {days} days — keep it alive",
                &[("age", &age), ("days", &days)],
            ),
            Tone::Danger,
        ),
        _ => (age, Tone::Muted),
    };
    CardLimits {
        budget,
        state,
        activity,
        activity_tone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn status() -> AccountStatus {
        AccountStatus {
            account: "gd".into(),
            kind: "drive".into(),
            crypts: vec![],
            daily_upload_limit: Some(1000),
            daily_upload_is_default: true,
            uploaded_24h: 250,
            next_release_unix: Some(7_200),
            paused_until_unix: None,
            pause_reason: None,
            last_activity_unix: Some(0),
            inactive_days: Some(0),
            inactivity_warn_days: Some(548),
            inactivity_is_default: true,
            inactivity: "fine",
            last_keepalive_unix: None,
            bwlimit: None,
            tpslimit: None,
            max_uploads: None,
        }
    }

    #[test]
    fn budget_bar_reset_pause_and_activity() {
        crate::gui::i18n::set_language(crate::gui::i18n::Language::English);
        let mut s = status();
        let card = card_limits(&s, 3_600);
        let budget = card.budget.unwrap();
        assert_eq!((budget.ratio, budget.bar.as_str()), (0.25, "25%"));
        assert_eq!(budget.line, "Uploaded (24 h): 250 B of 1000 B");
        assert_eq!(
            card.state,
            Some((
                "Oldest uploads leave the 24 h window in 1 h 00 min".into(),
                Tone::Muted
            ))
        );
        assert_eq!(
            (card.activity.as_str(), card.activity_tone),
            ("Last activity 1h ago", Tone::Muted)
        );

        s.pause_reason = Some("budget");
        s.paused_until_unix = Some(3_600 + 3 * 3_600 + 300);
        assert_eq!(
            card_limits(&s, 3_600).state,
            Some((
                "Daily upload limit reached — uploads resume in 3 h 05 min".into(),
                Tone::Warning
            ))
        );
        s.pause_reason = Some("provider");
        assert!(card_limits(&s, 3_600)
            .state
            .unwrap()
            .0
            .starts_with("Provider upload limit"));

        s.inactivity = "exceeded";
        s.inactive_days = Some(600);
        s.last_activity_unix = None;
        let card = card_limits(&s, 3_600);
        assert_eq!(card.activity_tone, Tone::Danger);
        assert_eq!(
            card.activity,
            "No activity from this computer yet · over 548 days — keep it alive"
        );
        s.inactivity = "near";
        assert_eq!(card_limits(&s, 3_600).activity_tone, Tone::Warning);

        // No budget: no bar and no window line.
        s.daily_upload_limit = None;
        s.pause_reason = None;
        let card = card_limits(&s, 3_600);
        assert!(card.budget.is_none() && card.state.is_none());
        assert_eq!(wait(0), "under 1 min");
        assert_eq!(wait(59), "1 min");
    }
}
