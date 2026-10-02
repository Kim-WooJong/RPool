//! Human-readable speed test report (the `--json` report is the model).
use super::model::{ConcurrencyTuning, RemoteSpeed, SpeedTestReport};

const MIB: f64 = 1024.0 * 1024.0;

/// Decimal units per second, as providers advertise them: MB/s from 1 MB/s,
/// KB/s below (so a slow account never shows as "0.0").
fn mb_s(rate: Option<f64>) -> String {
    rate.map_or_else(
        || "-".into(),
        |r| {
            if r >= 1e6 {
                format!("{:.1} MB/s", r / 1e6)
            } else {
                format!("{:.0} KB/s", r / 1e3)
            }
        },
    )
}

/// Throughput, plus files per second when many small files were moved
/// (there the per-file round trips, not bandwidth, decide).
fn transfer_rate(rate: Option<f64>, seconds: Option<f64>, files: usize) -> String {
    match seconds.filter(|_| files >= MANY_FILES && rate.is_some()) {
        Some(seconds) => format!(
            "{} ({:.1} files/s)",
            mb_s(rate),
            files as f64 / seconds.max(1e-3)
        ),
        None => mb_s(rate),
    }
}

/// From this many files per remote the table also shows files per second.
const MANY_FILES: usize = 16;

fn ms(value: Option<u64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v} ms"))
}

fn status(remote: &RemoteSpeed) -> String {
    match (&remote.error, remote.ok) {
        (None, true) => "ok".into(),
        (Some(error), _) => format!("failed: {error}"),
        (None, false) => "failed".into(),
    }
}

fn rate_of(report: &SpeedTestReport, remote: &str, upload: bool) -> String {
    report
        .remotes
        .iter()
        .find(|r| r.remote == remote)
        .map(|r| {
            mb_s(if upload {
                r.upload_bytes_per_s
            } else {
                r.download_bytes_per_s
            })
        })
        .unwrap_or_else(|| "-".into())
}

pub(crate) fn render(report: &SpeedTestReport) -> String {
    let mut out = String::new();
    let target = report
        .pool
        .as_ref()
        .map_or_else(|| "remotes".to_owned(), |p| format!("pool {p}"));
    out.push_str(&format!(
        "Speed test of {target}: {:.1} MiB per remote in {} file(s), {} parallel; remotes tested one after another\n",
        report.bytes_per_remote as f64 / MIB,
        report.files_per_remote,
        report.parallel
    ));
    let header = [
        "REMOTE".to_owned(),
        "BACKEND".to_owned(),
        "FIRST OP".to_owned(),
        "LATENCY".to_owned(),
        "UPLOAD".to_owned(),
        "DOWNLOAD".to_owned(),
        "STATUS".to_owned(),
    ];
    let rows: Vec<[String; 7]> = report
        .remotes
        .iter()
        .map(|r| {
            [
                r.remote.clone(),
                r.backend.clone().unwrap_or_else(|| "-".into()),
                ms(r.first_op_ms),
                ms(r.latency_ms),
                transfer_rate(
                    r.upload_bytes_per_s,
                    r.upload_seconds,
                    report.files_per_remote,
                ),
                transfer_rate(
                    r.download_bytes_per_s,
                    r.download_seconds,
                    report.files_per_remote,
                ),
                status(r),
            ]
        })
        .collect();
    let mut widths = header.clone().map(|h| h.chars().count());
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 7]| {
        let mut text = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let pad = widths[i].saturating_sub(cell.chars().count());
            if i == 6 {
                text.push_str(cell);
            } else if (2..=5).contains(&i) {
                text.push_str(&" ".repeat(pad));
                text.push_str(cell);
                text.push_str("  ");
            } else {
                text.push_str(cell);
                text.push_str(&" ".repeat(pad + 2));
            }
        }
        text.trim_end().to_owned()
    };
    out.push_str(&line(&header));
    out.push('\n');
    for row in &rows {
        out.push_str(&line(row));
        out.push('\n');
    }
    let basis = if report.pool.is_some() {
        "effect on the pool"
    } else {
        "slowest"
    };
    match &report.bottleneck_upload {
        Some(remote) => out.push_str(&format!(
            "Upload bottleneck ({basis}): {remote} ({})\n",
            rate_of(report, remote, true)
        )),
        None => out.push_str("Upload bottleneck: none (no remote succeeded)\n"),
    }
    match &report.bottleneck_download {
        Some(remote) => out.push_str(&format!(
            "Download bottleneck ({basis}): {remote} ({})\n",
            rate_of(report, remote, false)
        )),
        None => out.push_str("Download bottleneck: none (no remote succeeded)\n"),
    }
    if let Some(e) = &report.estimate {
        let coding = if e.parity_shards == 0 {
            "no parity".to_owned()
        } else {
            format!("RS {}+{}", e.data_shards, e.parity_shards)
        };
        out.push_str(&format!(
            "Pool estimate ({coding}): upload {}, download {} of file data\n",
            mb_s(Some(e.upload_bytes_per_s)),
            mb_s(Some(e.download_bytes_per_s))
        ));
    } else if report.pool.is_some() {
        out.push_str("Pool estimate: unavailable (not every remote succeeded)\n");
    }
    out.push_str(&tuning(report));
    if !report.leftovers.is_empty() {
        out.push_str("Test folders that could not be deleted (safe to remove by hand):\n");
        for path in &report.leftovers {
            out.push_str(&format!("  {path}\n"));
        }
    }
    out
}

/// The `--tune-uploads` / `--tune-downloads` lines: rate per level and the
/// recommendation.
fn tuning(report: &SpeedTestReport) -> String {
    let mut out = tuning_section(report, "uploads", "--max-uploads", |r| {
        r.upload_tuning.as_ref()
    });
    out.push_str(&tuning_section(
        report,
        "downloads",
        "--max-downloads",
        |r| r.download_tuning.as_ref(),
    ));
    out
}

fn tuning_section(
    report: &SpeedTestReport,
    what: &str,
    flag: &str,
    pick: impl Fn(&RemoteSpeed) -> Option<&ConcurrencyTuning>,
) -> String {
    let tuned: Vec<_> = report
        .remotes
        .iter()
        .filter_map(|r| pick(r).map(|t| (r, t)))
        .collect();
    let Some((_, first)) = tuned.first() else {
        return String::new();
    };
    let mut out = format!(
        "Simultaneous shard {what} ({:.0} MiB shards):\n",
        first.file_bytes as f64 / MIB
    );
    for (remote, tuning) in &tuned {
        let mut steps: Vec<String> = tuning
            .steps
            .iter()
            .map(|s| match (&s.error, s.bytes_per_s) {
                (Some(error), _) => format!("{}: failed ({error})", s.parallel),
                (None, rate) => format!("{}: {}", s.parallel, mb_s(rate)),
            })
            .collect();
        if let Some(error) = &tuning.error {
            steps.push(format!("failed ({error})"));
        }
        let now = tuning
            .current
            .map_or_else(|| "default".to_owned(), |n| n.to_string());
        let advice = match tuning.recommended {
            Some(n) => format!("recommended {n} (now {now})"),
            None => format!("no recommendation (now {now})"),
        };
        out.push_str(&format!(
            "  {}: {} -> {advice}\n",
            remote.remote,
            steps.join(", ")
        ));
        if let (Some(n), Some(name)) = (tuning.recommended, &remote_name(&remote.remote)) {
            if tuning.current != Some(n) {
                out.push_str(&format!(
                    "    apply: rpool provider limits set --remote {name} {flag} {n}\n"
                ));
            }
        }
    }
    out
}

fn remote_name(remote: &str) -> Option<String> {
    crate::storage::rclone::remote_name(remote)
        .ok()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::super::model::*;
    use super::*;

    pub(crate) fn sample() -> SpeedTestReport {
        let ok = |remote: &str, up: f64, down: f64| RemoteSpeed {
            remote: remote.into(),
            backend: Some("dropbox".into()),
            ok: true,
            error: None,
            first_op_ms: Some(1500),
            latency_ms: Some(120),
            upload_bytes_per_s: Some(up),
            download_bytes_per_s: Some(down),
            upload_seconds: Some(1.0),
            download_seconds: Some(1.0),
            verified: true,
            upload_tuning: None,
            download_tuning: None,
        };
        let mut failed = ok("sftp_crypt:rpool", 0.0, 0.0);
        failed.ok = false;
        failed.backend = Some("sftp".into());
        failed.error = Some("upload: permission denied: x".into());
        failed.upload_bytes_per_s = None;
        failed.download_bytes_per_s = None;
        failed.verified = false;
        SpeedTestReport {
            version: REPORT_VERSION,
            pool: Some("archive".into()),
            bytes_per_remote: 16 * 1024 * 1024,
            files_per_remote: 4,
            parallel: 4,
            remotes: vec![
                ok("dropbox_1_crypt:rpool", 12_500_000.0, 40_000_000.0),
                ok("d2:rpool", 3_000_000.0, 50_000_000.0),
                failed,
            ],
            estimate: None,
            bottleneck_upload: Some("d2:rpool".into()),
            bottleneck_download: Some("dropbox_1_crypt:rpool".into()),
            leftovers: vec!["sftp_crypt:rpool/.rpool-speedtest/00ff".into()],
        }
    }

    #[test]
    fn human_report_has_table_bottlenecks_and_leftovers() {
        let text = render(&sample());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with(
            "Speed test of pool archive: 16.0 MiB per remote in 4 file(s), 4 parallel"
        ));
        assert!(lines[1].starts_with("REMOTE"));
        assert!(lines[1].contains("FIRST OP") && lines[1].contains("DOWNLOAD"));
        assert!(
            lines[2].starts_with("dropbox_1_crypt:rpool  dropbox"),
            "{}",
            lines[2]
        );
        assert!(
            lines[2].contains("1500 ms") && lines[2].contains("12.5") && lines[2].ends_with("ok")
        );
        assert!(
            lines[4].contains("-") && lines[4].ends_with("failed: upload: permission denied: x")
        );
        // Columns line up.
        let col = lines[1].find("STATUS").unwrap();
        assert_eq!(&lines[2][col..], "ok");
        assert!(text.contains("Upload bottleneck (effect on the pool): d2:rpool (3.0 MB/s)"));
        assert!(text.contains(
            "Download bottleneck (effect on the pool): dropbox_1_crypt:rpool (40.0 MB/s)"
        ));
        assert!(text.contains("Pool estimate: unavailable"));
        assert!(text.contains("  sftp_crypt:rpool/.rpool-speedtest/00ff"));
    }

    #[test]
    fn tuning_lines_show_levels_recommendation_and_apply_command() {
        use super::super::model::{ConcurrencyTuning, TuningStep};
        let mut report = sample();
        assert!(!render(&report).contains("Simultaneous shard uploads"));
        let step = |parallel, rate: Option<f64>, error: Option<&str>| TuningStep {
            parallel,
            files: 4,
            bytes_per_s: rate,
            error: error.map(str::to_owned),
        };
        report.remotes[0].upload_tuning = Some(ConcurrencyTuning {
            account: Some("filen_1".into()),
            file_bytes: 1024 * 1024,
            steps: vec![
                step(1, Some(2e6), None),
                step(2, Some(4e6), None),
                step(4, None, Some("rate limited")),
            ],
            recommended: Some(2),
            current: None,
            error: None,
        });
        let text = render(&report);
        let remote = report.remotes[0].remote.clone();
        assert!(
            text.contains("Simultaneous shard uploads (1 MiB shards):"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "  {remote}: 1: 2.0 MB/s, 2: 4.0 MB/s, 4: failed (rate limited) -> recommended 2 (now default)"
            )),
            "{text}"
        );
        let name = remote.split(':').next().unwrap().to_owned();
        assert!(text.contains(&format!(
            "apply: rpool provider limits set --remote {name} --max-uploads 2"
        )));
        report.remotes[0].upload_tuning.as_mut().unwrap().current = Some(2);
        assert!(!render(&report).contains("apply:"), "already applied");
        let mut reads = report.remotes[0].upload_tuning.clone().unwrap();
        reads.current = None;
        report.remotes[0].download_tuning = Some(reads);
        let text = render(&report);
        assert!(
            text.contains("Simultaneous shard downloads (1 MiB shards):"),
            "{text}"
        );
        assert!(text.contains(&format!(
            "apply: rpool provider limits set --remote {name} --max-downloads 2"
        )));
    }

    #[test]
    fn estimate_and_no_pool_lines() {
        let mut report = sample();
        report.estimate = Some(PoolEstimate {
            data_shards: 4,
            parity_shards: 2,
            upload_bytes_per_s: 8_000_000.0,
            download_bytes_per_s: 90_000_000.0,
        });
        assert!(render(&report)
            .contains("Pool estimate (RS 4+2): upload 8.0 MB/s, download 90.0 MB/s of file data"));
        report.pool = None;
        report.estimate = None;
        report.leftovers.clear();
        report.bottleneck_upload = None;
        let text = render(&report);
        assert!(text.starts_with("Speed test of remotes:"));
        assert!(text.contains("Upload bottleneck: none"));
        assert!(text.contains("Download bottleneck (slowest)"));
        assert!(!text.contains("Pool estimate") && !text.contains("could not be deleted"));
    }

    #[test]
    fn json_round_trips_through_the_model() {
        let report = sample();
        let json = serde_json::to_string(&report).unwrap();
        let back: SpeedTestReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in [
            "version",
            "pool",
            "bytes_per_remote",
            "files_per_remote",
            "parallel",
            "remotes",
            "estimate",
            "bottleneck_upload",
            "bottleneck_download",
            "leftovers",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        assert_eq!(value["remotes"][0]["first_op_ms"], 1500);
        assert_eq!(value["remotes"][2]["ok"], false);
    }

    #[test]
    fn slow_rates_use_kb_and_many_files_show_files_per_second() {
        assert_eq!(mb_s(Some(40_000.0)), "40 KB/s");
        assert_eq!(mb_s(Some(2_500_000.0)), "2.5 MB/s");
        assert_eq!(
            transfer_rate(Some(40_000.0), Some(100.0), 64),
            "40 KB/s (0.6 files/s)"
        );
        assert_eq!(transfer_rate(Some(40_000.0), Some(100.0), 4), "40 KB/s");
    }
}
