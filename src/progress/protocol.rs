//! Wire format of the progress protocol (env flag, line prefix, JSON events).
use serde::{Deserialize, Serialize};

/// Environment variable the GUI sets on child processes to request progress events.
pub(crate) const PROGRESS_ENV: &str = "RPOOL_PROGRESS_PROTOCOL";
/// Prefix marking a stderr line as a progress event (followed by JSON).
pub(crate) const PROGRESS_PREFIX: &str = "@rpool-progress ";

/// Cumulative traffic of one storage in this CLI process (not a migration ETA).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StorageTraffic {
    pub remote: String,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub server_side_bytes: u64,
    pub upload_rate_10s: f64,
    pub download_rate_10s: f64,
    pub active_uploads: u32,
    pub active_downloads: u32,
    pub failed_ops: u64,
    pub retries: u64,
    #[serde(default)]
    pub last_upload_error: Option<String>,
    #[serde(default)]
    pub last_upload_storage_full: bool,
    #[serde(default)]
    pub capacity_no_space: bool,
}

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
    /// Per-storage traffic snapshot; later snapshots replace earlier ones.
    Traffic { remotes: Vec<StorageTraffic> },
    /// Operation finished.
    Finish,
}

/// Parses a stderr line into a [`ProgressEvent`]; returns `None` for lines
/// without the prefix or with invalid JSON. Used by `gui::task::runner`.
pub(crate) fn parse_line(line: &str) -> Option<ProgressEvent> {
    let payload = line.strip_prefix(PROGRESS_PREFIX)?;
    serde_json::from_str(payload).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_event_round_trips_below_the_runner_line_limit() {
        let row = StorageTraffic {
            remote: "account-with-a-long-name".into(),
            sent_bytes: 100,
            received_bytes: 200,
            server_side_bytes: 300,
            upload_rate_10s: 10.0,
            download_rate_10s: 20.0,
            active_uploads: 1,
            active_downloads: 2,
            failed_ops: 0,
            retries: 0,
            last_upload_error: None,
            last_upload_storage_full: false,
            capacity_no_space: false,
        };
        let event = ProgressEvent::Traffic {
            remotes: vec![row.clone(); 32],
        };
        let line = format!(
            "{PROGRESS_PREFIX}{}",
            serde_json::to_string(&event).unwrap()
        );
        assert!(line.len() < 16 * 1024);
        match parse_line(&line) {
            Some(ProgressEvent::Traffic { remotes }) => assert_eq!(remotes, vec![row; 32]),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
