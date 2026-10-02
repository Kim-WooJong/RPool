//! One line of the command history ([`TaskRecord`]), written by
//! `history::append` and shown by `rpool history` and the GUI dashboard.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A finished CLI command in the history file (JSON lines).
pub(crate) struct TaskRecord {
    /// 16-hex-digit id derived from operation, target, start time and pid.
    pub(crate) id: String,
    /// Short operation name, e.g. `put`, `pool-reprocess`.
    pub(crate) operation: String,
    /// Redacted target (path, manifest, pool), if any.
    pub(crate) target: Option<String>,
    /// Start time, Unix seconds.
    pub(crate) started_unix: u64,
    /// End time, Unix seconds.
    pub(crate) finished_unix: u64,
    /// `success` or `failed`.
    pub(crate) status: String,
    /// Redacted error text for failed commands.
    pub(crate) message: Option<String>,
}
