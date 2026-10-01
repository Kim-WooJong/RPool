//! Reads a `SpeedTestReport` from task output and turns it into what the
//! results list shows (formatted values, bars scaled to the fastest remote,
//! bottleneck flags).
use super::plan::mode_label;
use crate::gui::task::{LogKind, LogLine};
use crate::speedtest::model::SpeedTestReport;

/// A first operation slower than this is flagged "slow start".
pub(crate) const SLOW_START_MS: u64 = 5_000;

/// The report printed on stdout (one JSON document, compact or pretty).
pub(crate) fn parse_report(logs: &[LogLine]) -> Option<SpeedTestReport> {
    let stdout: Vec<&str> = logs
        .iter()
        .filter(|line| line.kind == LogKind::Stdout)
        .map(|line| line.text.as_str())
        .collect();
    // The last line that opens a JSON object and parses from there on.
    (0..stdout.len()).rev().find_map(|start| {
        if !stdout[start].trim_start().starts_with('{') {
            return None;
        }
        let text = stdout[start..].join("\n");
        serde_json::Deserializer::from_str(&text)
            .into_iter::<SpeedTestReport>()
            .next()?
            .ok()
    })
}

/// 12.3 MB/s, or 512 KB/s below 1 MB/s (decimal units, like most speed tools).
pub(crate) fn format_speed(bytes_per_s: f64) -> String {
    let value = bytes_per_s.max(0.0);
    if value >= 1_000_000.0 {
        format!("{:.1} MB/s", value / 1_000_000.0)
    } else {
        format!("{:.0} KB/s", value / 1_000.0)
    }
}

/// 850 ms, 31.2 s.
pub(crate) fn format_millis(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1_000.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bar {
    /// Share of the fastest remote's speed, 0..=1.
    pub(crate) ratio: f32,
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RemoteRow {
    pub(crate) remote: String,
    pub(crate) backend: Option<String>,
    pub(crate) ok: bool,
    pub(crate) error: Option<String>,
    pub(crate) first_op: Option<String>,
    pub(crate) slow_start: bool,
    pub(crate) latency: Option<String>,
    pub(crate) upload: Option<Bar>,
    pub(crate) download: Option<Bar>,
    pub(crate) slowest_upload: bool,
    pub(crate) slowest_download: bool,
}

impl RemoteRow {
    pub(crate) fn is_bottleneck(&self) -> bool {
        self.slowest_upload || self.slowest_download
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Estimate {
    pub(crate) upload: String,
    pub(crate) download: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReportView {
    /// "16 x 1 MiB" per account.
    pub(crate) mode: String,
    pub(crate) rows: Vec<RemoteRow>,
    pub(crate) estimate: Option<Estimate>,
    pub(crate) bottleneck_upload: Option<String>,
    pub(crate) bottleneck_download: Option<String>,
    pub(crate) leftovers: Vec<String>,
    /// Written by a newer RPool than this GUI knows.
    pub(crate) newer_version: bool,
}

/// From this many files per account the bars also show files per second
/// (there per-file round trips, not bandwidth, decide).
const MANY_FILES: usize = 16;

/// `(rate, seconds)` per remote; `files` per remote for the files/s note.
fn bars(values: Vec<(Option<f64>, Option<f64>)>, files: usize) -> Vec<Option<Bar>> {
    let max = values.iter().filter_map(|v| v.0).fold(0.0f64, f64::max);
    values
        .into_iter()
        .map(|(value, seconds)| {
            value.map(|value| Bar {
                ratio: if max > 0.0 {
                    (value / max).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                },
                text: match seconds.filter(|_| files >= MANY_FILES) {
                    Some(seconds) => {
                        let rate = format!("{:.1}", files as f64 / seconds.max(1e-3));
                        format!(
                            "{} · {}",
                            format_speed(value),
                            crate::gui::i18n::trf("{n} files/s", &[("n", &rate)])
                        )
                    }
                    None => format_speed(value),
                },
            })
        })
        .collect()
}

impl ReportView {
    pub(crate) fn new(report: &SpeedTestReport) -> Self {
        let uploads = bars(
            report
                .remotes
                .iter()
                .map(|r| (r.upload_bytes_per_s, r.upload_seconds))
                .collect(),
            report.files_per_remote,
        );
        let downloads = bars(
            report
                .remotes
                .iter()
                .map(|r| (r.download_bytes_per_s, r.download_seconds))
                .collect(),
            report.files_per_remote,
        );
        let is = |name: &Option<String>, remote: &str| name.as_deref() == Some(remote);
        let rows = report
            .remotes
            .iter()
            .zip(uploads.into_iter().zip(downloads))
            .map(|(r, (upload, download))| RemoteRow {
                remote: r.remote.clone(),
                backend: r.backend.clone(),
                ok: r.ok,
                error: r.error.clone(),
                first_op: r.first_op_ms.map(format_millis),
                slow_start: r.first_op_ms.is_some_and(|ms| ms > SLOW_START_MS),
                latency: r.latency_ms.map(format_millis),
                upload,
                download,
                slowest_upload: is(&report.bottleneck_upload, &r.remote),
                slowest_download: is(&report.bottleneck_download, &r.remote),
            })
            .collect();
        Self {
            mode: mode_label(report.files_per_remote, report.bytes_per_remote),
            rows,
            estimate: report.estimate.as_ref().map(|e| Estimate {
                upload: format_speed(e.upload_bytes_per_s),
                download: format_speed(e.download_bytes_per_s),
            }),
            bottleneck_upload: report.bottleneck_upload.clone(),
            bottleneck_download: report.bottleneck_download.clone(),
            leftovers: report.leftovers.clone(),
            newer_version: report.version > crate::speedtest::model::REPORT_VERSION,
        }
    }
}
