//! Transfer speed measurement (work package D) for the migration ETA.
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteSpeed {
    pub remote: String,
    pub upload_mib_s: Option<f64>,
    pub download_mib_s: Option<f64>,
    /// Why a measurement is missing.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SpeedReport {
    pub remotes: Vec<RemoteSpeed>,
    /// Aggregate speeds with `workers` parallel transfers, MiB/s.
    pub upload_mib_s: Option<f64>,
    pub download_mib_s: Option<f64>,
}

/// Measures each remote by writing, reading back and deleting one temporary
/// object of `sample_bytes` under `<remote>/.rpool-sync/bench/`. Only that
/// object is ever written or deleted.
pub(crate) fn measure(
    rclone: &str,
    remotes: &[String],
    sample_bytes: u64,
    workers: usize,
) -> Result<SpeedReport> {
    let _ = (rclone, remotes, sample_bytes, workers);
    bail!("speed measurement not implemented yet")
}

/// Duration range (fast, slow) in seconds for moving `download_bytes` and
/// `upload_bytes` at the given speeds; None when a speed is unknown.
pub(crate) fn estimate_seconds(
    download_bytes: u64,
    upload_bytes: u64,
    download_mib_s: Option<f64>,
    upload_mib_s: Option<f64>,
) -> Option<(f64, f64)> {
    let _ = (download_bytes, upload_bytes, download_mib_s, upload_mib_s);
    None
}
