//! JSON contract of `rpool … speed-test --json` (shared by CLI and GUI).
use serde::{Deserialize, Serialize};

pub(crate) const REPORT_VERSION: u32 = 1;
/// Folder under each remote that holds a run's test files (removed after).
pub(crate) const TEST_DIR: &str = ".rpool-speedtest";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SpeedTestReport {
    pub version: u32,
    /// Set for `pool speed-test`.
    pub pool: Option<String>,
    /// Bytes written to (and read from) each remote.
    pub bytes_per_remote: u64,
    /// Files per remote; they are transferred in parallel like shards.
    pub files_per_remote: usize,
    /// Parallel transfers per remote (the pool's workers, capped).
    pub parallel: usize,
    pub remotes: Vec<RemoteSpeed>,
    /// Only for a pool whose remotes all succeeded.
    pub estimate: Option<PoolEstimate>,
    /// Slowest remote for writing / reading (by its effect on the pool when
    /// a pool is given, else by raw throughput). `None` if no remote succeeded.
    pub bottleneck_upload: Option<String>,
    pub bottleneck_download: Option<String>,
    /// Test folders that could not be deleted (the user may remove them).
    pub leftovers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteSpeed {
    /// Remote address as configured in the pool (e.g. `dropbox_1_crypt:rpool`).
    pub remote: String,
    /// Backend type of the storage under it (`dropbox`, `sftp`, …) if known.
    pub backend: Option<String>,
    /// True when every step (write, read back, verify) succeeded.
    pub ok: bool,
    /// First error, one line, no secrets.
    pub error: Option<String>,
    /// The slower of the first metadata call and the first read on this
    /// remote in this process: provider cold start (e.g. ~30 s on some
    /// providers), paid once per RPool process. Milliseconds.
    pub first_op_ms: Option<u64>,
    /// Median of later small operations, milliseconds.
    pub latency_ms: Option<u64>,
    pub upload_bytes_per_s: Option<f64>,
    pub download_bytes_per_s: Option<f64>,
    pub upload_seconds: Option<f64>,
    pub download_seconds: Option<f64>,
    /// Read-back bytes matched what was written.
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PoolEstimate {
    pub data_shards: usize,
    pub parity_shards: usize,
    /// Expected plaintext upload rate of the pool: shards go to every remote
    /// in parallel, so the remote with the largest (share / speed) decides.
    pub upload_bytes_per_s: f64,
    /// Expected plaintext download rate (data shards only).
    pub download_bytes_per_s: f64,
}
