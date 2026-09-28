use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct QuotaReport {
    pub(crate) remote: String,
    pub(crate) total: Option<u64>,
    pub(crate) used: Option<u64>,
    pub(crate) free: Option<u64>,
    pub(crate) trashed: Option<u64>,
    pub(crate) other: Option<u64>,
    pub(crate) used_percent: Option<f64>,
    pub(crate) error: Option<String>,
}
