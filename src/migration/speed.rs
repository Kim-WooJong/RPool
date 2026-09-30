//! Transfer speed measurement (work package D) for the migration ETA.
//!
//! # What `measure` touches
//! For every remote it writes exactly one object
//! `<remote root>/.rpool-sync/bench/<32 random hex>.bin`, reads it back in full
//! and deletes exactly that address (`rclone deletefile`, never a purge or a
//! directory operation). The write goes through the same gated rclone crypt
//! path as legacy uploads (`RcloneContext::write_raw`: the crypt/no-data-
//! encryption gate, then `rclone rcat`), the read through `rclone cat`. Native
//! crypt pools upload ciphertext to the crypt base instead; the byte volume on
//! the wire is the same, so this is a realistic proxy (the local encryption
//! cost is not included, which the ETA safety factor covers).
//!
//! # Aggregate model (conservative)
//! Migration spreads shards roughly evenly over the pool's remotes, and each
//! transfer is one stream. For the `n` remotes that measured successfully with
//! single-stream speeds `s_i` and `w` workers, moving `B` bytes evenly takes at
//! least `max(Σ(B/n / s_i) / w, max_i(B/n / s_i))` (work-conserving scheduling,
//! but never faster than the slowest remote's share), so
//!
//! ```text
//! model = n / max(Σ(1/s_i) / w, 1 / min_i s_i)
//! ```
//!
//! Remotes are measured in parallel (up to `w` at a time) in separate upload
//! and download phases, so the local link is shared during the benchmark just
//! as during the migration. The observed phase throughput
//! `phase = n·sample / phase wall time` is the single-link cap. The aggregate
//! is `min(model, phase)`. Remotes that failed are left out (their error is
//! reported); with no successful remote the aggregate is `None`.
use crate::prelude::*;
use crate::storage::error::StorageError;
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::{OperationContext, WriteOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// The tests drive a fake rclone shell script.
#[cfg(all(test, unix))]
#[path = "speed_tests.rs"]
mod tests;

const MIB: f64 = 1024.0 * 1024.0;
/// Slow bound of the ETA: retries, crypt overhead, verification stalls.
pub(crate) const ETA_SAFETY_FACTOR: f64 = 1.5;
/// Directory (relative to a remote root) holding benchmark objects.
pub(crate) const BENCH_DIR: &str = ".rpool-sync/bench";

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

/// One remote's benchmark state across the phases.
struct Probe {
    remote: String,
    /// Bench object address; None when the root could not be resolved.
    address: Option<String>,
    upload: Option<Duration>,
    download: Option<Duration>,
    /// Whether a write was attempted (so the object may exist).
    written: bool,
    errors: Vec<String>,
}

/// Measures each remote by writing, reading back and deleting one temporary
/// object of `sample_bytes` under `<remote>/.rpool-sync/bench/`. Only that
/// object is ever written or deleted.
///
/// Per-remote failures are reported in `RemoteSpeed::error`; only invalid
/// input (`sample_bytes == 0`) fails the whole call. Speeds include rclone
/// process start-up, so small samples under-report (conservative); 16–64 MiB
/// is a sensible sample.
pub(crate) fn measure(
    rclone: &str,
    remotes: &[String],
    sample_bytes: u64,
    workers: usize,
) -> Result<SpeedReport> {
    if sample_bytes == 0 {
        bail!("speed sample size must be positive");
    }
    let size = usize::try_from(sample_bytes).context("speed sample too large")?;
    let workers = workers.max(1);
    let context = RcloneContext::inherited(rclone);
    let data = sample_data(size)?;
    let digest = blake3::hash(&data);

    let mut probes: Vec<Mutex<Probe>> = remotes
        .iter()
        .map(|remote| {
            let (address, errors) = match bench_address(remote) {
                Ok(address) => (Some(address), Vec::new()),
                Err(error) => (None, vec![format!("remote root: {error:#}")]),
            };
            Mutex::new(Probe {
                remote: remote.clone(),
                address,
                upload: None,
                download: None,
                written: false,
                errors,
            })
        })
        .collect();

    let upload_wall = run_phase(&probes, workers, |probe| {
        let Some(address) = probe.address.clone() else {
            return;
        };
        // Refused destinations (non-crypt, encryption off) are never written,
        // so they are not cleaned up either.
        if let Err(error) = context.ensure_crypt(&transfer_context(0), &address) {
            probe.errors.push(format!("upload: {error}"));
            return;
        }
        probe.written = true;
        let started = Instant::now();
        let mut source: &[u8] = &data;
        match context.write_raw(
            &transfer_context(sample_bytes),
            &address,
            &mut source,
            Some(sample_bytes),
            &WriteOptions::default(),
        ) {
            Ok(receipt) if receipt.size == sample_bytes => probe.upload = Some(started.elapsed()),
            Ok(receipt) => probe.errors.push(format!(
                "upload: wrote {} of {sample_bytes} bytes",
                receipt.size
            )),
            Err(error) => probe.errors.push(format!("upload: {error}")),
        }
    });

    let download_wall = run_phase(&probes, workers, |probe| {
        let (Some(address), Some(_)) = (probe.address.clone(), probe.upload) else {
            return;
        };
        let started = Instant::now();
        let mut sink = VerifySink::new(sample_bytes);
        match context.read_raw(&transfer_context(sample_bytes), &address, None, &mut sink) {
            Ok(_) if sink.count == sample_bytes && sink.hasher.finalize() == digest => {
                probe.download = Some(started.elapsed());
            }
            Ok(_) => probe
                .errors
                .push("readback: content does not match the uploaded sample".into()),
            Err(error) => probe.errors.push(format!("readback: {error}")),
        }
    });

    run_phase(&probes, workers, |probe| {
        let (Some(address), true) = (probe.address.clone(), probe.written) else {
            return;
        };
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(120));
        if let Err(error) = context.delete_raw(&ctx, &address) {
            // A failed mutation never reports NotFound; ask a read-only stat
            // whether anything is actually left (e.g. the upload never landed).
            let gone = matches!(
                context.stat_raw(&ctx, &address),
                Err(StorageError::NotFound { .. })
            );
            if !gone {
                probe.errors.push(format!(
                    "cleanup failed, bench object may remain at {address}: {error}"
                ));
            }
        }
    });

    let probes: Vec<Probe> = probes
        .drain(..)
        .map(|probe| probe.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect();
    let rate = |d: Option<Duration>| d.map(|d| mib_per_s(sample_bytes, d));
    let uploads: Vec<f64> = probes.iter().filter_map(|p| rate(p.upload)).collect();
    let downloads: Vec<f64> = probes.iter().filter_map(|p| rate(p.download)).collect();
    Ok(SpeedReport {
        upload_mib_s: aggregate(
            &uploads,
            workers,
            phase_rate(sample_bytes, uploads.len(), upload_wall),
        ),
        download_mib_s: aggregate(
            &downloads,
            workers,
            phase_rate(sample_bytes, downloads.len(), download_wall),
        ),
        remotes: probes
            .into_iter()
            .map(|p| RemoteSpeed {
                upload_mib_s: rate(p.upload),
                download_mib_s: rate(p.download),
                error: (!p.errors.is_empty()).then(|| p.errors.join("; ")),
                remote: p.remote,
            })
            .collect(),
    })
}

fn bench_address(remote: &str) -> Result<String> {
    let root = crate::remote_root::apply_remote_root(remote)?;
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|e| anyhow!("random bench name: {e}"))?;
    let name: String = id.iter().map(|b| format!("{b:02x}")).collect();
    Ok(crate::utils::remote_join(
        &root,
        &format!("{BENCH_DIR}/{name}.bin"),
    ))
}

/// Incompressible sample: a BLAKE3 XOF stream under a random key.
fn sample_data(size: usize) -> Result<Vec<u8>> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow!("random sample: {e}"))?;
    let mut data = vec![0u8; size];
    blake3::Hasher::new_keyed(&key)
        .finalize_xof()
        .fill(&mut data);
    Ok(data)
}

/// Generous deadline so a hung remote cannot stall the benchmark forever:
/// one minute plus the sample at 256 KiB/s.
fn transfer_context(sample_bytes: u64) -> OperationContext {
    let seconds = 60 + sample_bytes / (256 * 1024);
    OperationContext::with_deadline(Instant::now() + Duration::from_secs(seconds))
}

/// Runs `step` for every probe on up to `workers` threads; returns wall time.
fn run_phase(
    probes: &[Mutex<Probe>],
    workers: usize,
    step: impl Fn(&mut Probe) + Sync,
) -> Duration {
    let started = Instant::now();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers.min(probes.len()) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(probe) = probes.get(index) else {
                    break;
                };
                let mut probe = probe.lock().unwrap_or_else(|e| e.into_inner());
                step(&mut probe);
            });
        }
    });
    started.elapsed()
}

/// Streams readback into a hash, refusing more bytes than were uploaded.
struct VerifySink {
    hasher: Hasher,
    count: u64,
    limit: u64,
}
impl VerifySink {
    fn new(limit: u64) -> Self {
        Self {
            hasher: Hasher::new(),
            count: 0,
            limit,
        }
    }
}
impl Write for VerifySink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit - self.count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "readback longer than the uploaded sample",
            ));
        }
        self.hasher.update(bytes);
        self.count += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn mib_per_s(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / MIB / elapsed.as_secs_f64().max(1e-3)
}

fn phase_rate(sample_bytes: u64, successes: usize, wall: Duration) -> Option<f64> {
    (successes > 0).then(|| mib_per_s(sample_bytes * successes as u64, wall))
}

/// `min(n / max(Σ(1/s_i)/w, 1/min s_i), phase_cap)`; see the module docs.
pub(crate) fn aggregate(speeds: &[f64], workers: usize, phase_cap: Option<f64>) -> Option<f64> {
    let speeds: Vec<f64> = speeds
        .iter()
        .copied()
        .filter(|s| s.is_finite() && *s > 0.0)
        .collect();
    if speeds.is_empty() {
        return None;
    }
    let n = speeds.len() as f64;
    let inverse_sum: f64 = speeds.iter().map(|s| 1.0 / s).sum();
    let slowest = speeds.iter().copied().fold(f64::INFINITY, f64::min);
    let model = n / (inverse_sum / workers.max(1) as f64).max(1.0 / slowest);
    Some(match phase_cap {
        Some(cap) if cap.is_finite() && cap > 0.0 => model.min(cap),
        _ => model,
    })
}

/// Duration range `(fast, slow)` in seconds for moving `download_bytes` and
/// `upload_bytes` at the given speeds; None when a speed is unknown.
///
/// With `t_d = download_bytes / (download_mib_s · 2^20)` and
/// `t_u = upload_bytes / (upload_mib_s · 2^20)`:
///
/// ```text
/// fast = max(t_d, t_u)                        // downloads and uploads fully overlap
/// slow = (t_d + t_u) · ETA_SAFETY_FACTOR      // no overlap, +50% retries/crypt/stalls
/// ```
///
/// A side with zero bytes needs no speed and contributes 0 s. A needed speed
/// that is missing, zero, negative or not finite gives None.
pub(crate) fn estimate_seconds(
    download_bytes: u64,
    upload_bytes: u64,
    download_mib_s: Option<f64>,
    upload_mib_s: Option<f64>,
) -> Option<(f64, f64)> {
    let side = |bytes: u64, speed: Option<f64>| -> Option<f64> {
        if bytes == 0 {
            return Some(0.0);
        }
        let speed = speed.filter(|s| s.is_finite() && *s > 0.0)?;
        let seconds = bytes as f64 / (speed * MIB);
        seconds.is_finite().then_some(seconds)
    };
    let download = side(download_bytes, download_mib_s)?;
    let upload = side(upload_bytes, upload_mib_s)?;
    Some((
        download.max(upload),
        (download + upload) * ETA_SAFETY_FACTOR,
    ))
}

/// Human text for an ETA: "about 12–20 min", "about 3–5 h", "about 25 min – 2 h",
/// "under 1 min". The fast bound rounds down, the slow bound up. Units: minutes
/// below 90 min, hours below 48 h, then days. Invalid input gives "unknown".
pub(crate) fn format_duration_range(fast_seconds: f64, slow_seconds: f64) -> String {
    if !fast_seconds.is_finite() || !slow_seconds.is_finite() || fast_seconds < 0.0 {
        return "unknown".into();
    }
    let (fast, slow) = if fast_seconds <= slow_seconds {
        (fast_seconds, slow_seconds)
    } else {
        (slow_seconds, fast_seconds)
    };
    if slow < 60.0 {
        return "under 1 min".into();
    }
    let (fast_value, fast_unit) = in_unit(fast, false);
    let (slow_value, slow_unit) = in_unit(slow, true);
    if fast_unit == slow_unit {
        if fast_value >= slow_value {
            return format!("about {slow_value} {slow_unit}");
        }
        return format!("about {fast_value}–{slow_value} {slow_unit}");
    }
    format!("about {fast_value} {fast_unit} – {slow_value} {slow_unit}")
}

/// Formats an `estimate_seconds` result; None gives "unknown".
pub(crate) fn format_estimate(estimate: Option<(f64, f64)>) -> String {
    estimate.map_or_else(
        || "unknown".into(),
        |(fast, slow)| format_duration_range(fast, slow),
    )
}

fn in_unit(seconds: f64, round_up: bool) -> (u64, &'static str) {
    let (scale, unit) = if seconds < 90.0 * 60.0 {
        (60.0, "min")
    } else if seconds < 48.0 * 3600.0 {
        (3600.0, "h")
    } else {
        (86400.0, "d")
    };
    let value = seconds / scale;
    let rounded = if round_up {
        value.ceil()
    } else {
        value.floor()
    };
    (rounded.max(1.0) as u64, unit)
}
