//! Wire format of the progress protocol (env flag, line prefix, JSON events).
use serde::{Deserialize, Serialize};

/// Environment variable the GUI sets on child processes to request progress events.
pub(crate) const PROGRESS_ENV: &str = "RPOOL_PROGRESS_PROTOCOL";
/// Prefix marking a stderr line as a progress event (followed by JSON).
pub(crate) const PROGRESS_PREFIX: &str = "@rpool-progress ";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
/// One progress event, serialized as JSON tagged by `event` (snake_case).
pub(crate) enum ProgressEvent {
    /// Operation start with the total planned work.
    Start {
        /// Total bytes the operation expects to process.
        total_bytes: u64,
    },
    /// Incremental byte progress.
    Advance {
        /// Bytes of the planned total completed so far.
        completed_bytes: u64,
        /// Bytes actually transferred since the previous event (for rate display).
        transferred_bytes: u64,
    },
    /// Item-count progress for per-file operations.
    Items {
        /// Items finished so far.
        completed_items: usize,
        /// Total items in the operation.
        total_items: usize,
    },
    /// Operation finished.
    Finish,
}

/// Parses a stderr line into a [`ProgressEvent`]; returns `None` for lines
/// without the prefix or with invalid JSON. Used by `gui::task::runner`.
pub(crate) fn parse_line(line: &str) -> Option<ProgressEvent> {
    let payload = line.strip_prefix(PROGRESS_PREFIX)?;
    serde_json::from_str(payload).ok()
}
