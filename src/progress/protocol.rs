use serde::{Deserialize, Serialize};

pub(crate) const PROGRESS_ENV: &str = "RPOOL_PROGRESS_PROTOCOL";
pub(crate) const PROGRESS_PREFIX: &str = "@rpool-progress ";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum ProgressEvent {
    Start { total_bytes: u64 },
    Advance {
        completed_bytes: u64,
        transferred_bytes: u64,
    },
    Items {
        completed_items: usize,
        total_items: usize,
    },
    Finish,
}

pub(crate) fn parse_line(line: &str) -> Option<ProgressEvent> {
    let payload = line.strip_prefix(PROGRESS_PREFIX)?;
    serde_json::from_str(payload).ok()
}
