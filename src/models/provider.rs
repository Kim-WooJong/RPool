//! Result row of `rpool provider health` ([`ProviderHealthReport`]), built by
//! `provider::health`.
use super::QuotaReport;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
/// Reachability, latency and quota of one remote.
pub(crate) struct ProviderHealthReport {
    /// Checked remote.
    pub(crate) remote: String,
    /// The remote answered the probe.
    pub(crate) accessible: bool,
    /// Probe round-trip time, milliseconds.
    pub(crate) latency_ms: u128,
    /// Quota of the account behind the remote.
    pub(crate) quota: QuotaReport,
    /// Probe error, if any.
    pub(crate) error: Option<String>,
}
