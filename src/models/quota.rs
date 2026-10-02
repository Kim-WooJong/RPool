//! Account space report ([`QuotaReport`]) parsed from `rclone about --json`
//! (`storage::admin::quota`); used by provider health, placement and usage.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
/// Space of the account behind one remote; `None` values were not reported.
pub(crate) struct QuotaReport {
    /// Remote the report is for.
    pub(crate) remote: String,
    /// Account size in bytes (derived from used + free when missing).
    pub(crate) total: Option<u64>,
    /// Used bytes.
    pub(crate) used: Option<u64>,
    /// Free bytes.
    pub(crate) free: Option<u64>,
    /// Bytes in the provider's trash.
    pub(crate) trashed: Option<u64>,
    /// Bytes used by other services of the account.
    pub(crate) other: Option<u64>,
    /// `used / total * 100`.
    pub(crate) used_percent: Option<f64>,
    /// Why the report is incomplete (query or parse error).
    pub(crate) error: Option<String>,
}
