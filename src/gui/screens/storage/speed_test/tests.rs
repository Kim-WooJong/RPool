use super::plan::{self, Plan, PlanError, Preset};
use super::view::{format_millis, format_speed, parse_report, ReportView};
use super::{SpeedTestForm, Target};
use crate::gui::state::GuiState;
use crate::gui::task::{LogKind, LogLine};
use crate::speedtest::model::SpeedTestReport;
use std::ffi::OsString;

/// 4 remotes: a fast one, a slow-start one that is the upload bottleneck,
/// one that is the download bottleneck, and one that failed.
pub(crate) const SAMPLE: &str = r#"{
  "version": 1,
  "pool": "family",
  "bytes_per_remote": 16777216,
  "files_per_remote": 4,
  "parallel": 4,
  "remotes": [
    {"remote": "dropbox_1_crypt:rpool", "backend": "dropbox", "ok": true, "error": null,
     "first_op_ms": 820, "latency_ms": 140, "upload_bytes_per_s": 24500000.0,
     "download_bytes_per_s": 61000000.0, "upload_seconds": 0.68, "download_seconds": 0.27,
     "verified": true},
    {"remote": "google_drive_backup_account_2_crypt:rpool", "backend": "drive", "ok": true,
     "error": null, "first_op_ms": 31200, "latency_ms": 410, "upload_bytes_per_s": 640000.0,
     "download_bytes_per_s": 30500000.0, "upload_seconds": 26.2, "download_seconds": 0.55,
     "verified": true},
    {"remote": "nas_sftp_crypt:rpool", "backend": "sftp", "ok": true, "error": null,
     "first_op_ms": 95, "latency_ms": 12, "upload_bytes_per_s": 11800000.0,
     "download_bytes_per_s": 2400000.0, "upload_seconds": 1.42, "download_seconds": 6.99,
     "verified": true},
    {"remote": "onedrive_old_crypt:rpool", "backend": "onedrive", "ok": false,
     "error": "upload failed: 401 Unauthorized (token expired; reconnect the account)",
     "first_op_ms": 2100, "latency_ms": null, "upload_bytes_per_s": null,
     "download_bytes_per_s": null, "upload_seconds": null, "download_seconds": null,
     "verified": false}
  ],
  "estimate": null,
  "bottleneck_upload": "google_drive_backup_account_2_crypt:rpool",
  "bottleneck_download": "nas_sftp_crypt:rpool",
  "leftovers": ["onedrive_old_crypt:rpool/.rpool-speedtest/20261001-101500-3f2a"]
}"#;

fn sample() -> SpeedTestReport {
    serde_json::from_str(SAMPLE).unwrap()
}

fn line(kind: LogKind, text: &str) -> LogLine {
    LogLine {
        kind,
        text: text.into(),
    }
}

#[test]
fn report_is_read_from_pretty_stdout_between_other_output() {
    let mut logs = vec![
        line(LogKind::Stderr, "testing dropbox_1_crypt:rpool (1/4)"),
        line(LogKind::Stdout, "speed test finished"),
    ];
    logs.extend(SAMPLE.lines().map(|text| line(LogKind::Stdout, text)));
    logs.push(line(LogKind::Stderr, "{not json on stderr"));
    let report = parse_report(&logs).unwrap();
    assert_eq!(report, sample());
    assert_eq!(report.remotes.len(), 4);
    // Compact one-line JSON too.
    let compact = serde_json::to_string(&report).unwrap();
    assert_eq!(
        parse_report(&[line(LogKind::Stdout, &compact)]),
        Some(report)
    );
    assert_eq!(parse_report(&[line(LogKind::Stdout, "{ broken")]), None);
}

#[test]
fn view_scales_bars_flags_bottlenecks_and_formats() {
    let view = ReportView::new(&sample());
    assert_eq!(view.mode, "4 x 4 MiB");
    let [fast, slow_start, sftp, failed] = &view.rows[..] else {
        panic!("4 rows");
    };
    let up = |r: &super::view::RemoteRow| r.upload.as_ref().map(|b| (b.ratio, b.text.clone()));
    let down = |r: &super::view::RemoteRow| r.download.as_ref().map(|b| (b.ratio, b.text.clone()));
    assert_eq!(up(fast), Some((1.0, "24.5 MB/s".into())));
    assert_eq!(down(fast), Some((1.0, "61.0 MB/s".into())));
    let (ratio, text) = up(slow_start).unwrap();
    assert!((ratio - 0.64 / 24.5).abs() < 1e-4);
    assert_eq!(text, "640 KB/s");
    assert_eq!(down(sftp).unwrap().1, "2.4 MB/s");
    assert!(slow_start.slow_start && !fast.slow_start && !failed.slow_start);
    assert_eq!(slow_start.first_op.as_deref(), Some("31.2 s"));
    assert_eq!(fast.latency.as_deref(), Some("140 ms"));
    assert!(slow_start.slowest_upload && !slow_start.slowest_download);
    assert!(sftp.slowest_download && !sftp.slowest_upload);
    assert!(!fast.is_bottleneck() && !failed.is_bottleneck());
    assert!(!failed.ok && failed.upload.is_none() && failed.download.is_none());
    assert!(failed.error.as_deref().unwrap().contains("401"));
    assert_eq!(view.leftovers.len(), 1);
    assert!(view.estimate.is_none() && !view.newer_version);

    let mut report = sample();
    report.estimate = Some(crate::speedtest::model::PoolEstimate {
        data_shards: 2,
        parity_shards: 1,
        upload_bytes_per_s: 1_280_000.0,
        download_bytes_per_s: 999_999.0,
    });
    report.version += 1;
    let view = ReportView::new(&report);
    let estimate = view.estimate.unwrap();
    assert_eq!(
        (estimate.upload.as_str(), estimate.download.as_str()),
        ("1.3 MB/s", "1000 KB/s")
    );
    assert!(view.newer_version);
}

#[test]
fn formatting_units() {
    assert_eq!(format_speed(0.0), "0 KB/s");
    assert_eq!(format_speed(999_499.0), "999 KB/s");
    assert_eq!(format_speed(1_000_000.0), "1.0 MB/s");
    assert_eq!(format_speed(123_456_789.0), "123.5 MB/s");
    assert_eq!(format_millis(0), "0 ms");
    assert_eq!(format_millis(999), "999 ms");
    assert_eq!(format_millis(1_000), "1.0 s");
    assert_eq!(plan::binary_size(64 * 1024), "64 KiB");
    assert_eq!(plan::binary_size(4096 * plan::MIB), "4 GiB");
    assert_eq!(plan::binary_size(3 * plan::MIB / 2), "1.5 MiB");
}

#[test]
fn presets_and_validation() {
    let custom = Plan {
        size_mib: 16,
        files: 4,
    };
    let p = |preset, large, small| plan::preset_plan(preset, large, small, custom);
    assert_eq!(p(Preset::Quick, 0, 0), custom);
    assert_eq!(p(Preset::LargeFile, 4096, 0).mode_label(), "1 x 4 GiB");
    assert_eq!(p(Preset::ManySmall, 0, 256).mode_label(), "256 x 64 KiB");
    assert_eq!(p(Preset::ManySmall, 0, 256).size_mib, 16);
    assert_eq!(p(Preset::ManySmall, 0, 64).size_mib, 4);
    assert_eq!(p(Preset::ManySmall, 0, 1024).size_mib, 64);
    for mib in plan::LARGE_SIZES_MIB {
        assert!(p(Preset::LargeFile, mib, 0).validate().is_ok());
    }
    for count in plan::SMALL_COUNTS {
        assert!(p(Preset::ManySmall, 0, count).validate().is_ok());
    }
    let plan = |size_mib, files| Plan { size_mib, files }.validate();
    assert_eq!(plan(0, 1), Err(PlanError::Size));
    assert_eq!(plan(4097, 1), Err(PlanError::Size));
    assert_eq!(plan(16, 0), Err(PlanError::Files));
    assert_eq!(plan(4096, 4097), Err(PlanError::Files));
    // 1 MiB / 512 files = 2 KiB per file.
    assert_eq!(plan(1, 512), Err(PlanError::FileTooSmall));
    assert!(plan(1, 256).is_ok(), "exactly 4 KiB per file");
    assert!(plan(4096, 4096).is_ok());
    assert!(!Plan {
        size_mib: 512,
        files: 1
    }
    .needs_confirmation());
    assert!(Plan {
        size_mib: 512,
        files: 1
    }
    .is_large());
    assert!(Plan {
        size_mib: 1024,
        files: 1
    }
    .needs_confirmation());
    assert!(!Plan {
        size_mib: 16,
        files: 4
    }
    .is_large());
}

fn strings(args: Vec<OsString>) -> Vec<String> {
    args.into_iter().map(|a| a.into_string().unwrap()).collect()
}

#[test]
fn command_lines_always_pass_size_and_files() {
    let large = Plan {
        size_mib: 4096,
        files: 1,
    };
    assert_eq!(
        strings(plan::pool_args("-family pool", large)),
        [
            "pool",
            "speed-test",
            "--size-mib",
            "4096",
            "--files",
            "1",
            "--json",
            "--",
            "-family pool"
        ]
    );
    let remotes = vec!["a_crypt:rpool".to_string(), "-b crypt:폴더".to_string()];
    let small = plan::preset_plan(Preset::ManySmall, 0, 256, large);
    assert_eq!(
        strings(plan::provider_args(&remotes, small)),
        [
            "provider",
            "speed-test",
            "--remote=a_crypt:rpool",
            "--remote=-b crypt:폴더",
            "--size-mib",
            "16",
            "--files",
            "256",
            "--json"
        ]
    );
    assert_eq!(
        strings(super::args(&Target::Pool("p".into()), &remotes, large))[..2],
        ["pool", "speed-test"]
    );
    assert_eq!(
        strings(super::args(&Target::Remotes, &remotes, large))[..2],
        ["provider", "speed-test"]
    );
}

/// Layout fixture: the pool "family" with the sample result, a large plan
/// waiting for confirmation, and the same result on the Providers page.
pub(crate) fn with_sample_result(state: &mut GuiState) {
    let report = sample();
    let remotes: Vec<String> = report.remotes.iter().map(|r| r.remote.clone()).collect();
    let pool = crate::models::PoolDefinition {
        remotes: remotes.clone(),
        ..Default::default()
    };
    state.pool_definitions.insert("family".into(), pool);
    state.pools.selected = "family".into();
    state.crypt_remotes = remotes.clone();
    let form: &mut SpeedTestForm = &mut state.speed_test;
    form.preset = Preset::LargeFile;
    form.large_mib = 4096;
    form.remotes = remotes;
    form.confirming = Some(Target::Pool("family".into()));
    let mut view = ReportView::new(&report);
    form.results.insert(Target::Remotes, view.clone());
    view.estimate = Some(super::view::Estimate {
        upload: "1.9 MB/s".into(),
        download: "7.1 MB/s".into(),
    });
    form.results.insert(Target::Pool("family".into()), view);
    form.notices.insert(
        Target::Pool("family".into()),
        crate::gui::i18n::tr("Some accounts failed the speed test; see the errors below.").into(),
    );
}

/// The command lines the GUI builds are accepted by the real CLI.
#[test]
fn gui_command_lines_parse_with_the_cli() {
    use crate::cli::pool::{PoolArgs, PoolCommands};
    use crate::cli::{Cli, Commands, ProviderArgs, ProviderCommands};
    use clap::Parser;
    let with_binary = |args: Vec<OsString>| {
        let mut all = vec![OsString::from("rpool")];
        all.extend(args);
        Cli::try_parse_from(all).unwrap().command
    };
    let large = Plan {
        size_mib: 4096,
        files: 1,
    };
    match with_binary(plan::pool_args("-odd pool", large)) {
        Some(Commands::Pool(PoolArgs {
            command: PoolCommands::SpeedTest { name, size },
        })) => {
            assert_eq!(name, "-odd pool");
            assert_eq!(
                (size.size_mib, size.files, size.json),
                (4096, Some(1), true)
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    let small = Plan {
        size_mib: 16,
        files: 256,
    };
    let remotes = vec!["a_crypt:x".to_string(), "-b:".to_string()];
    match with_binary(plan::provider_args(&remotes, small)) {
        Some(Commands::Provider(ProviderArgs {
            command: ProviderCommands::SpeedTest { remotes: got, size },
        })) => {
            assert_eq!(got, remotes);
            assert_eq!(
                (size.size_mib, size.files, size.json),
                (16, Some(256), true)
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}
