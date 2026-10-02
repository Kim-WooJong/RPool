//! Daily upload budget of one account: how much of the rolling 24 h window
//! is used, whether uploads may start, and when a paused account resumes.
use super::ledger::AccountUsage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why an account is paused for uploads.
pub(crate) enum PauseReason {
    /// RPool's own count reached the configured budget.
    Budget,
    /// The provider refused an upload for its upload limit.
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Whether an account may start new uploads right now.
pub(crate) enum Admission {
    /// Uploads may start.
    Open,
    /// Uploads wait until unix time `until` for the given `reason`.
    Paused {
        /// Unix time (seconds) when uploads may start again.
        until: u64,
        /// Why: RPool's own upload budget or the provider's upload limit.
        reason: PauseReason,
    },
}

/// Point-in-time budget of one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BudgetView {
    /// Bytes counted in the rolling 24 h window at the evaluation time.
    pub used: u64,
    /// Configured daily budget in bytes (`None` = unlimited).
    pub limit: Option<u64>,
    /// When the oldest counted bytes leave the window (`None` = nothing counted).
    pub next_release: Option<u64>,
    /// Open or paused (with resume time); consumed by `limits_view` and the runtime limiter.
    pub admission: Admission,
}

/// Budget of `usage` (none recorded = nothing uploaded) under `limit` at
/// `now`. A provider pause outranks the local count; with a limit, uploads
/// pause once the window holds `limit` bytes and resume when enough old
/// bytes have left it to drop below the limit.
pub(crate) fn evaluate(usage: Option<&AccountUsage>, limit: Option<u64>, now: u64) -> BudgetView {
    let empty = AccountUsage::default();
    let usage = usage.unwrap_or(&empty);
    let used = usage.used(now);
    let next_release = usage.live(now).map(|(at, _)| at).next();
    let admission = match usage.provider_pause_until.filter(|until| *until > now) {
        Some(until) => Admission::Paused {
            until,
            reason: PauseReason::Provider,
        },
        None => match limit {
            Some(limit) if used >= limit => {
                let mut remaining = used;
                let until = usage
                    .live(now)
                    .find_map(|(at, bytes)| {
                        remaining -= bytes;
                        (remaining < limit).then_some(at)
                    })
                    .unwrap_or(now);
                Admission::Paused {
                    until,
                    reason: PauseReason::Budget,
                }
            }
            _ => Admission::Open,
        },
    };
    BudgetView {
        used,
        limit,
        next_release,
        admission,
    }
}

/// "3 h 05 min", "12 min", "under 1 min".
pub(crate) fn wait_text(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    match minutes {
        0 => "under 1 min".into(),
        1..=59 => format!("{minutes} min"),
        _ => format!("{} h {:02} min", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3600;

    #[test]
    fn exhausted_budget_pauses_until_enough_bytes_leave_the_window() {
        let mut usage = AccountUsage::default();
        let start = 100 * HOUR;
        usage.add(start, 40);
        usage.add(start + HOUR, 30);
        usage.add(start + 2 * HOUR, 30);
        let now = start + 3 * HOUR;
        let open = evaluate(Some(&usage), Some(101), now);
        assert_eq!((open.used, open.admission), (100, Admission::Open));
        assert_eq!(open.next_release, Some(AccountUsage::expires_at(100)));
        // At exactly the limit, uploads wait; dropping the first bucket (40)
        // brings 60 < 100 back under it.
        let paused = evaluate(Some(&usage), Some(100), now);
        assert_eq!(
            paused.admission,
            Admission::Paused {
                until: AccountUsage::expires_at(100),
                reason: PauseReason::Budget
            }
        );
        // A tighter limit needs the second bucket to go as well.
        let tight = evaluate(Some(&usage), Some(35), now);
        assert!(
            matches!(tight.admission, Admission::Paused { until, .. } if until == AccountUsage::expires_at(101))
        );
        // After the resume time the account is open again.
        let later = evaluate(Some(&usage), Some(100), AccountUsage::expires_at(100));
        assert_eq!((later.used, later.admission), (60, Admission::Open));
        assert_eq!(evaluate(Some(&usage), None, now).admission, Admission::Open);
        assert_eq!(evaluate(None, Some(1), now).admission, Admission::Open);
    }

    #[test]
    fn provider_pause_wins_until_it_expires() {
        let usage = AccountUsage {
            provider_pause_until: Some(500),
            ..Default::default()
        };
        assert_eq!(
            evaluate(Some(&usage), None, 499).admission,
            Admission::Paused {
                until: 500,
                reason: PauseReason::Provider
            }
        );
        assert_eq!(evaluate(Some(&usage), None, 500).admission, Admission::Open);
        assert_eq!(wait_text(0), "under 1 min");
        assert_eq!(wait_text(61), "2 min");
        assert_eq!(wait_text(3 * HOUR + 300), "3 h 05 min");
    }
}
