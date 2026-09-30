use super::*;
use std::os::unix::fs::PermissionsExt;

const MB: u64 = 1024 * 1024;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9 * a.abs().max(1.0)
}

// ---- estimate_seconds -------------------------------------------------------

#[test]
fn estimate_basic_range() {
    // 100 MiB down at 10 MiB/s = 10 s, 50 MiB up at 5 MiB/s = 10 s.
    let (fast, slow) = estimate_seconds(100 * MB, 50 * MB, Some(10.0), Some(5.0)).unwrap();
    assert!(close(fast, 10.0));
    assert!(close(slow, 20.0 * ETA_SAFETY_FACTOR));
    let (fast, slow) = estimate_seconds(100 * MB, 10 * MB, Some(10.0), Some(10.0)).unwrap();
    assert!(close(fast, 10.0));
    assert!(close(slow, 11.0 * ETA_SAFETY_FACTOR));
    assert!(fast <= slow);
}

#[test]
fn estimate_zero_bytes_needs_no_speed() {
    assert_eq!(estimate_seconds(0, 0, None, None), Some((0.0, 0.0)));
    let (fast, slow) = estimate_seconds(0, 20 * MB, None, Some(2.0)).unwrap();
    assert!(close(fast, 10.0) && close(slow, 10.0 * ETA_SAFETY_FACTOR));
    let (fast, slow) = estimate_seconds(20 * MB, 0, Some(2.0), Some(0.0)).unwrap();
    assert!(close(fast, 10.0) && close(slow, 15.0));
}

#[test]
fn estimate_unknown_or_invalid_speed_is_none() {
    assert_eq!(estimate_seconds(1, 0, None, Some(1.0)), None);
    assert_eq!(estimate_seconds(0, 1, Some(1.0), None), None);
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(estimate_seconds(MB, MB, Some(bad), Some(1.0)), None);
        assert_eq!(estimate_seconds(MB, MB, Some(1.0), Some(bad)), None);
    }
}

#[test]
fn estimate_tiny_speed_and_huge_bytes_stay_finite() {
    let (fast, slow) = estimate_seconds(u64::MAX, u64::MAX, Some(1e-6), Some(1e-6)).unwrap();
    assert!(fast.is_finite() && slow.is_finite() && fast > 1e15 && slow > fast);
    // Subnormal speed overflows to infinity: reported as unknown, not inf.
    assert_eq!(
        estimate_seconds(u64::MAX, 0, Some(f64::MIN_POSITIVE), None),
        None
    );
}

// ---- formatting -------------------------------------------------------------

#[test]
fn format_ranges() {
    assert_eq!(
        format_duration_range(12.0 * 60.0, 20.0 * 60.0),
        "about 12–20 min"
    );
    assert_eq!(
        format_duration_range(3.0 * 3600.0, 5.0 * 3600.0),
        "about 3–5 h"
    );
    assert_eq!(
        format_duration_range(3.2 * 3600.0, 4.1 * 3600.0),
        "about 3–5 h"
    );
    assert_eq!(
        format_duration_range(25.0 * 60.0, 2.0 * 3600.0),
        "about 25 min – 2 h"
    );
    assert_eq!(
        format_duration_range(3.0 * 86400.0, 4.5 * 86400.0),
        "about 3–5 d"
    );
    assert_eq!(format_duration_range(10.0, 30.0), "under 1 min");
    assert_eq!(format_duration_range(0.0, 0.0), "under 1 min");
    assert_eq!(format_duration_range(10.0, 70.0), "about 1–2 min");
    assert_eq!(format_duration_range(600.0, 600.0), "about 10 min");
    // Reversed bounds are tolerated.
    assert_eq!(
        format_duration_range(20.0 * 60.0, 12.0 * 60.0),
        "about 12–20 min"
    );
}

#[test]
fn format_invalid_and_option() {
    assert_eq!(format_duration_range(f64::NAN, 1.0), "unknown");
    assert_eq!(format_duration_range(1.0, f64::INFINITY), "unknown");
    assert_eq!(format_duration_range(-1.0, 100.0), "unknown");
    assert_eq!(format_estimate(None), "unknown");
    assert_eq!(
        format_estimate(estimate_seconds(600 * MB, 0, Some(1.0), None)),
        "about 10–15 min"
    );
}

// ---- aggregate model --------------------------------------------------------

#[test]
fn aggregate_model() {
    assert_eq!(aggregate(&[], 4, Some(10.0)), None);
    assert_eq!(aggregate(&[0.0, f64::NAN], 4, None), None);
    // Enough workers: bounded by the slowest remote's even share.
    assert!(close(aggregate(&[10.0, 10.0, 2.0], 8, None).unwrap(), 6.0));
    // One worker: harmonic behaviour, 3 / (0.1 + 0.1 + 0.5).
    assert!(close(
        aggregate(&[10.0, 10.0, 2.0], 1, None).unwrap(),
        3.0 / 0.7
    ));
    // Equal speeds scale with parallelism up to the remote count.
    assert!(close(aggregate(&[5.0; 4], 2, None).unwrap(), 10.0));
    assert!(close(aggregate(&[5.0; 4], 16, None).unwrap(), 20.0));
    // Observed link throughput caps the model.
    assert!(close(aggregate(&[5.0; 4], 16, Some(12.0)).unwrap(), 12.0));
    assert!(close(aggregate(&[5.0; 4], 16, Some(0.0)).unwrap(), 20.0));
}

// ---- measure with a fake rclone --------------------------------------------

/// Fake rclone over a local directory. `c*` remotes are crypt, `plain` is not.
/// Remote names containing `failup` break mid-upload (leaving a partial
/// object), `corrupt` return altered readback, `nodelete` refuse deletion.
fn fake_rclone(dir: &Path) -> PathBuf {
    let root = dir.join("store");
    fs::create_dir_all(&root).unwrap();
    let script = format!(
        r#"#!/bin/sh
ROOT='{root}'
verb="$1"
for last; do :; done
path="$ROOT/$(printf '%s' "$last" | sed 's/:/\//')"
case "$verb" in
  config) printf '%s' '{{"c1":{{"type":"crypt"}},"c2":{{"type":"crypt"}},"cfailup":{{"type":"crypt"}},"ccorrupt":{{"type":"crypt"}},"cnodelete":{{"type":"crypt"}},"plain":{{"type":"s3"}}}}' ;;
  rcat)
    mkdir -p "$(dirname "$path")"
    case "$last" in
      *failup*) head -c 1000 > "$path"; cat > /dev/null; exit 1 ;;
      *) cat > "$path" ;;
    esac ;;
  cat)
    [ -f "$path" ] || exit 3
    case "$last" in
      *corrupt*) printf 'X'; tail -c +2 "$path" ;;
      *) cat "$path" ;;
    esac ;;
  deletefile)
    case "$last" in *nodelete*) exit 1 ;; esac
    [ -f "$path" ] || exit 4
    rm "$path" ;;
  lsjson)
    [ -f "$path" ] || exit 3
    printf '{{"Size":%s,"IsDir":false}}' "$(wc -c < "$path" | tr -d ' ')" ;;
  *) exit 2 ;;
esac
"#,
        root = root.display()
    );
    let path = dir.join("rclone");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(files_under(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn measure_fake_rclone_success_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let rclone = fake_rclone(dir.path());
    let remotes = vec!["c1:root".to_string(), "c2:root/sub".to_string()];
    let report = measure(rclone.to_str().unwrap(), &remotes, 256 * 1024, 2).unwrap();
    assert_eq!(report.remotes.len(), 2);
    for (speed, remote) in report.remotes.iter().zip(&remotes) {
        assert_eq!(&speed.remote, remote);
        assert_eq!(speed.error, None, "{speed:?}");
        assert!(speed.upload_mib_s.unwrap() > 0.0);
        assert!(speed.download_mib_s.unwrap() > 0.0);
    }
    assert!(report.upload_mib_s.unwrap() > 0.0);
    assert!(report.download_mib_s.unwrap() > 0.0);
    let store = dir.path().join("store");
    assert!(store.join("c1/root/.rpool-sync/bench").is_dir());
    assert!(store.join("c2/root/sub/.rpool-sync/bench").is_dir());
    assert_eq!(files_under(&store), Vec::<PathBuf>::new());
}

#[test]
fn measure_fake_rclone_failures_are_per_remote() {
    let dir = tempfile::tempdir().unwrap();
    let rclone = fake_rclone(dir.path());
    let store = dir.path().join("store");
    // Pre-existing object in the bench dir must survive: only our object is deleted.
    let keep = store.join("c1/r/.rpool-sync/bench/keep.bin");
    fs::create_dir_all(keep.parent().unwrap()).unwrap();
    fs::write(&keep, b"other").unwrap();
    let remotes: Vec<String> = [
        "c1:r",
        "cfailup:r",
        "ccorrupt:r",
        "plain:r",
        "missingconfig:r",
        "cnodelete:r",
    ]
    .map(String::from)
    .to_vec();
    let report = measure(rclone.to_str().unwrap(), &remotes, 64 * 1024, 3).unwrap();
    let by = |name: &str| report.remotes.iter().find(|r| r.remote == name).unwrap();

    let ok = by("c1:r");
    assert_eq!(ok.error, None);
    assert!(ok.upload_mib_s.is_some() && ok.download_mib_s.is_some());

    let failup = by("cfailup:r");
    assert!(failup.upload_mib_s.is_none() && failup.download_mib_s.is_none());
    let error = failup.error.as_deref().unwrap();
    assert!(error.starts_with("upload:"), "{error}");
    assert!(!error.contains("cleanup"), "{error}");

    let corrupt = by("ccorrupt:r");
    assert!(corrupt.upload_mib_s.is_some() && corrupt.download_mib_s.is_none());
    assert!(corrupt.error.as_deref().unwrap().contains("readback"));

    for refused in ["plain:r", "missingconfig:r"] {
        let r = by(refused);
        assert!(r.upload_mib_s.is_none() && r.error.as_deref().unwrap().contains("upload"));
    }

    let nodelete = by("cnodelete:r");
    assert!(nodelete.upload_mib_s.is_some() && nodelete.download_mib_s.is_some());
    assert!(nodelete
        .error
        .as_deref()
        .unwrap()
        .contains("cleanup failed, bench object may remain at cnodelete:r/.rpool-sync/bench/"));

    // Aggregates only use the successful measurements.
    assert!(report.upload_mib_s.is_some() && report.download_mib_s.is_some());

    // Only the intentionally undeletable bench object and the foreign file remain.
    let mut left = files_under(&store);
    left.sort();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&keep));
    assert!(left.iter().any(|p| p.starts_with(store.join("cnodelete"))));
    assert!(!store.join("plain").exists() && !store.join("missingconfig").exists());
}

#[test]
fn measure_rejects_empty_sample_and_handles_no_remotes() {
    assert!(measure("rclone", &["c1:r".into()], 0, 1).is_err());
    let report = measure("/nonexistent/rclone", &[], MB, 0).unwrap();
    assert!(report.remotes.is_empty());
    assert_eq!(report.upload_mib_s, None);
    assert_eq!(report.download_mib_s, None);
}

// ---- measure with real rclone over local crypt remotes ---------------------

/// Needs a real `rclone` on PATH (runs on the dev Mac or in the
/// `rpool-linux-dev` container): `cargo test migration::speed -- --ignored`.
#[test]
#[ignore = "needs a real rclone binary"]
fn measure_real_rclone_local_crypt() {
    let rclone = std::process::Command::new("sh")
        .args(["-c", "command -v rclone"])
        .output()
        .unwrap();
    let rclone = String::from_utf8(rclone.stdout).unwrap().trim().to_string();
    assert!(!rclone.is_empty(), "rclone not on PATH");
    let dir = tempfile::tempdir().unwrap();
    let obscured = std::process::Command::new(&rclone)
        .args(["obscure", "bench-test-password"])
        .output()
        .unwrap();
    let obscured = String::from_utf8(obscured.stdout)
        .unwrap()
        .trim()
        .to_string();
    let mut config = String::new();
    for name in ["b1", "b2", "b3"] {
        let base = dir.path().join(name);
        fs::create_dir_all(&base).unwrap();
        config.push_str(&format!(
            "[{name}]\ntype = crypt\nremote = {}\npassword = {obscured}\n\n",
            base.display()
        ));
    }
    let conf = dir.path().join("rclone.conf");
    fs::write(&conf, config).unwrap();
    let wrapper = dir.path().join("rclone-wrapper");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec '{rclone}' --config '{}' \"$@\"\n",
            conf.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();

    let remotes: Vec<String> = ["b1:pool", "b2:pool", "b3:pool"].map(String::from).to_vec();
    let report = measure(wrapper.to_str().unwrap(), &remotes, 4 * MB, 2).unwrap();
    eprintln!("{report:#?}");
    for speed in &report.remotes {
        assert_eq!(speed.error, None, "{speed:?}");
        assert!(speed.upload_mib_s.unwrap() > 0.0 && speed.download_mib_s.unwrap() > 0.0);
    }
    assert!(report.upload_mib_s.is_some() && report.download_mib_s.is_some());
    for name in ["b1", "b2", "b3"] {
        let base = dir.path().join(name);
        assert_eq!(files_under(&base), Vec::<PathBuf>::new(), "{name}");
        let listing = std::process::Command::new(&wrapper)
            .args(["lsf", "-R", &format!("{name}:pool/.rpool-sync/bench")])
            .output()
            .unwrap();
        assert!(listing.status.success());
        assert!(listing.stdout.is_empty(), "{name} bench dir not empty");
    }
}
