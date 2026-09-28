use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskRecord {
    pub(crate) id: String,
    pub(crate) operation: String,
    pub(crate) target: Option<String>,
    pub(crate) started_unix: u64,
    pub(crate) finished_unix: u64,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
}
