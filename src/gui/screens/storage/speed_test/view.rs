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
    /// The lowest measured speed of all accounts.
    pub(crate) slowest_upload: bool,
    pub(crate) slowest_download: bool,
    /// The account that limits the pool: largest share of the pool's shards
    /// per unit of speed. Not always the slowest one, since placement gives
    /// accounts different shares.
    pub(crate) limits_upload: bool,
    pub(crate) limits_download: bool,
    pub(crate) tuning: Option<TuningView>,
}

/// `--tune-uploads` result of one account.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TuningView {
    pub(crate) account: Option<String>,
    /// "1: 2.0 MB/s", "4: failed" in test order, with the failure if any.
    pub(crate) steps: Vec<(String, Option<String>)>,
    pub(crate) recommended: Option<usize>,
    /// The account's own limit (None = backend default).
    pub(crate) current: Option<usize>,
}

impl TuningView {
    fn new(tuning: &crate::speedtest::model::UploadTuning) -> Self {
        Self {
            account: tuning.account.clone(),
            steps: tuning
                .steps
                .iter()
                .map(|step| match (&step.error, step.bytes_per_s) {
                    (None, Some(rate)) => {
                        (format!("{}: {}", step.parallel, format_speed(rate)), None)
                    }
                    (error, _) => (
                        crate::gui::i18n::trf("{n}: failed", &[("n", &step.parallel)]),
                        error.clone(),
                    ),
                })
                .collect(),
            recommended: tuning.recommended,
            current: tuning.current,
        }
    }
}

impl RemoteRow {
    pub(crate) fn is_bottleneck(&self) -> bool {
        self.limits_upload || self.limits_download
    }
}

/// Index of the lowest usable speed among accounts that passed (ties: first).
fn slowest(rates: impl Iterator<Item = Option<f64>>) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, rate) in rates.enumerate() {
        let Some(rate) = rate.filter(|r| r.is_finite() && *r > 0.0) else {
            continue;
        };
        if best.is_none_or(|(_, b)| rate < b) {
            best = Some((i, rate));
        }
    }
    best.map(|(i, _)| i)
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
    /// After "Apply": every row of `account` now has its own limit `uploads`.
    pub(crate) fn applied(&mut self, account: &str, uploads: usize) {
        for tuning in self.rows.iter_mut().filter_map(|r| r.tuning.as_mut()) {
            if tuning.account.as_deref() == Some(account) {
                tuning.current = Some(uploads);
            }
        }
    }
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
        let ok = |rate: Option<f64>, passed: bool| rate.filter(|_| passed);
        let slowest_up = slowest(
            report
                .remotes
                .iter()
                .map(|r| ok(r.upload_bytes_per_s, r.ok)),
        );
        let slowest_down = slowest(
            report
                .remotes
                .iter()
                .map(|r| ok(r.download_bytes_per_s, r.ok)),
        );
        let rows = report
            .remotes
            .iter()
            .enumerate()
            .zip(uploads.into_iter().zip(downloads))
            .map(|((i, r), (upload, download))| RemoteRow {
                remote: r.remote.clone(),
                backend: r.backend.clone(),
                ok: r.ok,
                error: r.error.clone(),
                first_op: r.first_op_ms.map(format_millis),
                slow_start: r.first_op_ms.is_some_and(|ms| ms > SLOW_START_MS),
                latency: r.latency_ms.map(format_millis),
                upload,
                download,
                slowest_upload: slowest_up == Some(i),
                slowest_download: slowest_down == Some(i),
                limits_upload: is(&report.bottleneck_upload, &r.remote),
                limits_download: is(&report.bottleneck_download, &r.remote),
                tuning: r.upload_tuning.as_ref().map(TuningView::new),
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
