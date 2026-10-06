//! A bandwidth limit for this whole RPool process, set once at start by a
//! command that should move data slowly (`pool migrate run --bwlimit`).
//!
//! It caps the bytes RPool pipes itself (the global pacer buckets) and the
//! streams rclone copies between accounts on its own (`copy_object`): each of
//! those `rclone copyto` processes gets `--bwlimit` = limit / streams, so the
//! sum stays below the limit with up to `streams` copies at once. The limit
//! follows a `--bwlimit`-style timetable (e.g. fast at night).
use crate::storage::account::bandwidth::{Rate, Timetable};
use std::sync::OnceLock;

/// The process limit and how many copies may run at once.
static LIMIT: OnceLock<(Timetable, usize)> = OnceLock::new();

/// Sets the process limit; later calls are ignored (one command per process).
pub(crate) fn set(table: Timetable, streams: usize) {
    let _ = LIMIT.set((table, streams.max(1)));
}

/// The limit in force now; unlimited when none was set.
pub(crate) fn rate_now() -> Rate {
    LIMIT.get().map_or(Rate::OFF, |(table, _)| {
        table
            .at_unix(
                super::traffic::now_unix(),
                crate::storage::account::runtime::offset(),
            )
            .0
    })
}

/// `--bwlimit` value for one rclone copy stream now, if a limit is set.
pub(crate) fn per_stream_bwlimit() -> Option<String> {
    let (_, streams) = LIMIT.get()?;
    let rate = rate_now();
    if rate == Rate::OFF {
        return None;
    }
    let share = |r: Option<u64>| r.map(|b| (b / *streams as u64).max(1));
    Some(
        Rate {
            up: share(rate.up),
            down: share(rate.down),
        }
        .rclone_value(),
    )
}

/// The tighter of two limits (`None` = unlimited).
pub(crate) fn tighter(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tighter_keeps_the_smaller_limit() {
        assert_eq!(tighter(None, None), None);
        assert_eq!(tighter(Some(5), None), Some(5));
        assert_eq!(tighter(None, Some(7)), Some(7));
        assert_eq!(tighter(Some(5), Some(7)), Some(5));
    }
}
