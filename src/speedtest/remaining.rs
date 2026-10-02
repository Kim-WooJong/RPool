//! "Remaining at most N s": an upper bound of a run's remaining time,
//! printed on stderr whenever a remote or a tuning step starts. Tuning steps
//! have a bounded length ([`LEVEL_MAX`]); a remote's normal test is bounded
//! by the slowest one seen so far in this run (or [`NORMAL_DEFAULT`] before
//! the first). The GUI shows it instead of a rate-based estimate.
use super::options::TestPlan;
use super::progress::Position;
use super::tune::{LEVELS, WARM_UP, WINDOW};
use std::sync::Mutex;
use std::time::Duration;

/// Start of the line; the seconds follow.
pub(crate) const LINE_PREFIX: &str = "Remaining at most ";
/// Time for the shards in flight to finish after a level's window.
const DRAIN_ALLOWANCE: Duration = Duration::from_secs(10);
/// Longest tuning level: warm-up, window, finishing the shards in flight.
pub(crate) const LEVEL_MAX: Duration = WARM_UP
    .saturating_add(WINDOW)
    .saturating_add(DRAIN_ALLOWANCE);
/// Writing the download read set under the account's normal caps.
pub(crate) const READ_SET_MAX: Duration = Duration::from_secs(60);
/// A normal test before any has finished in this run.
const NORMAL_DEFAULT: Duration = Duration::from_secs(60);

/// Normal-test durations seen in this run.
#[derive(Default)]
pub(crate) struct Budget {
    normal: Mutex<Duration>,
}

impl Budget {
    pub(crate) fn record_normal(&self, took: Duration) {
        if let Ok(mut slowest) = self.normal.lock() {
            *slowest = (*slowest).max(took);
        }
    }
    /// Bound of one normal test: the slowest seen so far.
    pub(crate) fn normal(&self) -> Duration {
        match self.normal.lock().map(|d| *d) {
            Ok(d) if !d.is_zero() => d,
            _ => NORMAL_DEFAULT,
        }
    }
}

/// `levels` tuning levels.
pub(crate) fn levels(levels: usize) -> Duration {
    LEVEL_MAX.saturating_mul(u32::try_from(levels).unwrap_or(u32::MAX))
}

/// All of one remote's tuning.
pub(crate) fn tuning(plan: &TestPlan) -> Duration {
    let mut total = Duration::ZERO;
    if plan.tune_uploads {
        total += levels(LEVELS.len());
    }
    if plan.tune_downloads {
        total += READ_SET_MAX + levels(LEVELS.len());
    }
    total
}

/// The bound when the current remote still needs `current`: plus every
/// remote after it.
pub(crate) fn bound(
    at: &Position<'_>,
    plan: &TestPlan,
    budget: &Budget,
    current: Duration,
) -> Duration {
    let after = u32::try_from(at.count.saturating_sub(at.index)).unwrap_or(u32::MAX);
    current + (budget.normal() + tuning(plan)).saturating_mul(after)
}

pub(crate) fn line(bound: Duration) -> String {
    format!("{LINE_PREFIX}{} s", bound.as_secs())
}

/// Prints the bound (stderr; never fails the run).
pub(crate) fn say(at: &Position<'_>, plan: &TestPlan, budget: &Budget, current: Duration) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "{}",
        line(bound(at, plan, budget, current))
    );
}

/// The seconds of a [`line`].
pub(crate) fn parse(text: &str) -> Option<u64> {
    text.trim()
        .strip_prefix(LINE_PREFIX)?
        .strip_suffix(" s")?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_adds_the_remotes_after_the_current_one() {
        let mut plan = TestPlan {
            bytes_per_remote: 1,
            file_sizes: vec![1],
            parallel: 1,
            tune_uploads: true,
            tune_downloads: true,
            shard_bytes: 1,
        };
        let budget = Budget::default();
        let at = |index| Position {
            remote: "r",
            index,
            count: 3,
        };
        // Tuning per remote: 6 + 6 levels and the read set.
        let tune = levels(12) + READ_SET_MAX;
        assert_eq!(tuning(&plan), tune);
        // Before any normal test: 60 s each for the remotes after this one.
        assert_eq!(
            bound(&at(1), &plan, &budget, Duration::from_secs(5)),
            Duration::from_secs(5) + (NORMAL_DEFAULT + tune) * 2
        );
        budget.record_normal(Duration::from_secs(20));
        budget.record_normal(Duration::from_secs(12));
        assert_eq!(
            bound(&at(2), &plan, &budget, Duration::ZERO),
            Duration::from_secs(20) + tune,
            "the slowest normal test seen"
        );
        assert_eq!(
            bound(&at(3), &plan, &budget, Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        plan.tune_uploads = false;
        plan.tune_downloads = false;
        assert_eq!(tuning(&plan), Duration::ZERO);
    }

    #[test]
    fn lines_round_trip() {
        assert_eq!(parse(&line(Duration::from_secs(754))), Some(754));
        assert_eq!(parse("Remaining at most x s"), None);
        assert_eq!(parse("Testing r (1/2): latency"), None);
    }
}
