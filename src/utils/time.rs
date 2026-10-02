//! Wall-clock helper.
use crate::prelude::*;

/// Current unix time in seconds (0 if the clock is before 1970).
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
