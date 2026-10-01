//! Inactivity warnings: providers may delete accounts nobody used for a long
//! time. RPool can only report when *it* last reached the account; whether a
//! provider counts API access as activity is the provider's decision.
const DAY: u64 = 86_400;
/// Share of the threshold after which the age is shown as "near".
const NEAR_PERCENT: u64 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// No threshold for this account.
    Untracked,
    /// Never reached by RPool on this computer.
    Unknown,
    Fine,
    Near,
    Exceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Inactivity {
    pub level: Level,
    pub age_days: Option<u64>,
    pub warn_days: Option<u32>,
}

pub(crate) fn evaluate(last_activity: Option<u64>, warn_days: Option<u32>, now: u64) -> Inactivity {
    let age_days = last_activity.map(|at| now.saturating_sub(at) / DAY);
    let level = match (warn_days, age_days) {
        (None, _) => Level::Untracked,
        (Some(_), None) => Level::Unknown,
        (Some(warn), Some(age)) if age >= u64::from(warn) => Level::Exceeded,
        (Some(warn), Some(age)) if age * 100 >= u64::from(warn) * NEAR_PERCENT => Level::Near,
        _ => Level::Fine,
    };
    Inactivity {
        level,
        age_days,
        warn_days,
    }
}

/// Whether an automatic keep-alive is due: idle for `every_days` or longer
/// (or never seen). `0` disables it.
pub(crate) fn keepalive_due(last_activity: Option<u64>, every_days: u32, now: u64) -> bool {
    every_days > 0
        && last_activity.is_none_or(|at| now.saturating_sub(at) >= u64::from(every_days) * DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_grade_the_age() {
        let now = 1000 * DAY;
        let at = |days: u64| Some(now - days * DAY);
        assert_eq!(evaluate(at(5), None, now).level, Level::Untracked);
        assert_eq!(evaluate(None, Some(548), now).level, Level::Unknown);
        assert_eq!(evaluate(at(100), Some(548), now).level, Level::Fine);
        assert_eq!(evaluate(at(439), Some(548), now).level, Level::Near);
        assert_eq!(evaluate(at(438), Some(548), now).level, Level::Fine);
        let over = evaluate(at(548), Some(548), now);
        assert_eq!((over.level, over.age_days), (Level::Exceeded, Some(548)));
        // A clock behind the record is age 0, not an underflow.
        assert_eq!(evaluate(Some(now + 50), Some(10), now).age_days, Some(0));
    }

    #[test]
    fn keepalive_runs_after_the_interval_or_when_never_seen() {
        let now = 100 * DAY;
        assert!(keepalive_due(None, 7, now));
        assert!(!keepalive_due(Some(now - 6 * DAY), 7, now));
        assert!(keepalive_due(Some(now - 7 * DAY), 7, now));
        assert!(!keepalive_due(None, 0, now));
    }
}
