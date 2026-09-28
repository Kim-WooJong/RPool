use serde::Serialize;
use super::QuotaReport;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderHealthReport {
    pub(crate) remote: String,
    pub(crate) accessible: bool,
    pub(crate) latency_ms: u128,
    pub(crate) quota: QuotaReport,
    pub(crate) error: Option<String>,
}
