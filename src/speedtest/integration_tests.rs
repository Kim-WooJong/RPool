//! End to end with the installed rclone over temporary local crypt remotes
//! (never the user's config). `RPOOL_TEST_RCLONE` overrides the binary.
//! Run with `cargo test speedtest -- --include-ignored`.
use super::engine::Engine;
use super::model::SpeedTestReport;
use super::options::TestPlan;
use super::run::{execute, PoolTarget};
use crate::cli::pool::SpeedTestSizeArgs;
use crate::crypt::obscure::obscure;
use crate::prelude::*;
use crate::storage::rclone::{ConfigSelection, RcloneContext};
use std::sync::atomic::{AtomicBool, Ordering};

fn rclone() -> String {
    std::env::var("RPOOL_TEST_RCLONE").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone".into()
        } else {
            "rclone".into()
        }
    })
}

struct Fixture {
    temp: tempfile::TempDir,
    conf: PathBuf,
}

impl Fixture {
    /// `count` local bases `b<i>` with crypt remotes `c<i>` over them, plus
    /// `broken`: a crypt whose base path runs through a regular file.
    fn new(count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut text = String::new();
        for i in 1..=count {
            let dir = temp.path().join(format!("base{i}"));
            fs::create_dir(&dir).unwrap();
            text.push_str(&format!(
                "[b{i}]\ntype = local\n\n[c{i}]\ntype = crypt\nremote = b{i}:{}\npassword = {}\n\n",
                dir.display(),
                obscure(format!("speed secret {i}")).unwrap()
            ));
        }
        let blocker = temp.path().join("not-a-dir");
        fs::write(&blocker, b"x").unwrap();
        text.push_str(&format!(
            "[bx]\ntype = local\n\n[broken]\ntype = crypt\nremote = bx:{}\npassword = {}\n",
            blocker.join("sub").display(),
            obscure("broken secret").unwrap()
        ));
        let conf = temp.path().join("rclone.conf");
        fs::write(&conf, text).unwrap();
        Self { temp, conf }
    }
    fn engine(&self, native: bool) -> Engine {
        let context = RcloneContext::new(rclone().into(), ConfigSelection::File(self.conf.clone()));
        Engine::new(context, native, Arc::new(AtomicBool::new(false)))
    }
    /// Regular files left anywhere under base `i` (test data only lives there).
    fn files_in_base(&self, i: usize) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![self.temp.path().join(format!("base{i}"))];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
    /// Entries under `<remote>/.rpool-speedtest`, listed through rclone crypt.
    fn listed_test_dir(&self, remote: &str) -> String {
        let output = std::process::Command::new(rclone())
            .arg("--config")
            .arg(&self.conf)
            .args(["lsf", "-R", &format!("{remote}/.rpool-speedtest")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

fn plan(size_mib: u64, files: Option<u64>, workers: Option<usize>) -> TestPlan {
    TestPlan::new(
        &SpeedTestSizeArgs {
            size_mib,
            files,
            json: false,
        },
        workers,
    )
    .unwrap()
}

fn pool_of(remotes: &[String], native: bool) -> PoolDefinition {
    PoolDefinition {
        remotes: remotes.to_vec(),
        data_shards: 2,
        parity_shards: 1,
        workers: 3,
        native_crypt: native,
        ..PoolDefinition::default()
    }
}

fn assert_clean(fixture: &Fixture, report: &SpeedTestReport, bases: &[usize]) {
    assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
    for &i in bases {
        assert!(
            fixture.files_in_base(i).is_empty(),
            "{:?}",
            fixture.files_in_base(i)
        );
        assert_eq!(fixture.listed_test_dir(&format!("c{i}:pool")), "");
    }
}

fn assert_ok(report: &SpeedTestReport) {
    for remote in &report.remotes {
        assert!(remote.ok && remote.verified, "{remote:?}");
        assert!(remote.error.is_none());
        assert_eq!(remote.backend.as_deref(), Some("local"));
        assert!(remote.first_op_ms.is_some() && remote.latency_ms.is_some());
        assert!(remote.upload_bytes_per_s.unwrap() > 0.0);
        assert!(remote.download_bytes_per_s.unwrap() > 0.0);
        assert!(remote.upload_seconds.unwrap() > 0.0);
    }
}

fn remotes(count: usize) -> Vec<String> {
    (1..=count).map(|i| format!("c{i}:pool")).collect()
}

#[test]
#[ignore = "requires rclone"]
fn pool_of_three_local_crypt_remotes() {
    let fixture = Fixture::new(3);
    let remotes = remotes(3);
    let pool = pool_of(&remotes, false);
    let plan = plan(4, None, Some(pool.workers));
    let report = execute(
        &fixture.engine(false),
        &rclone(),
        &remotes,
        &plan,
        Some(PoolTarget {
            name: "p",
            definition: &pool,
        }),
    )
    .unwrap();
    assert_eq!(report.pool.as_deref(), Some("p"));
    assert_eq!(report.bytes_per_remote, 4 << 20);
    assert_eq!((report.files_per_remote, report.parallel), (1, 1));
    assert_ok(&report);
    let estimate = report.estimate.as_ref().expect("estimate");
    assert_eq!((estimate.data_shards, estimate.parity_shards), (2, 1));
    assert!(estimate.upload_bytes_per_s > 0.0 && estimate.download_bytes_per_s > 0.0);
    assert!(remotes.contains(report.bottleneck_upload.as_ref().unwrap()));
    // RS 2+1 over 3 round-robin remotes: c3 holds only parity, so it never
    // limits reads.
    assert_ne!(report.bottleneck_download.as_deref(), Some("c3:pool"));
    assert_clean(&fixture, &report, &[1, 2, 3]);
    // The JSON contract round-trips (floats up to serde_json's last-digit
    // parsing precision).
    let json = serde_json::to_string(&report).unwrap();
    let back: SpeedTestReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.remotes.len(), 3);
    assert_eq!(back.bottleneck_upload, report.bottleneck_upload);
    let (a, b) = (back.estimate.unwrap(), estimate);
    assert!((a.upload_bytes_per_s - b.upload_bytes_per_s).abs() <= 1e-6 * b.upload_bytes_per_s);
}

#[test]
#[ignore = "requires rclone"]
fn broken_remote_fails_alone() {
    let fixture = Fixture::new(2);
    let remotes = vec![
        "c1:pool".to_owned(),
        "broken:pool".to_owned(),
        "c2:pool".to_owned(),
    ];
    let report = execute(
        &fixture.engine(false),
        &rclone(),
        &remotes,
        &plan(1, Some(2), None),
        None,
    )
    .unwrap();
    let broken = &report.remotes[1];
    assert!(!broken.ok && !broken.verified, "{broken:?}");
    let error = broken.error.as_deref().unwrap();
    assert!(!error.contains('\n') && error.len() <= 210, "{error}");
    assert!(!error.contains("broken secret"));
    assert!(broken.upload_bytes_per_s.is_none());
    for i in [0, 2] {
        assert!(report.remotes[i].ok, "{:?}", report.remotes[i]);
    }
    assert!(report.estimate.is_none());
    assert!(report.bottleneck_upload.is_some());
    assert_ne!(report.bottleneck_upload.as_deref(), Some("broken:pool"));
    assert_clean(&fixture, &report, &[1, 2]);
}

#[test]
#[ignore = "requires rclone"]
fn plain_remote_is_refused_without_writing() {
    let fixture = Fixture::new(1);
    let plain = format!("b1:{}/plain", fixture.temp.path().join("base1").display());
    let report = execute(
        &fixture.engine(false),
        &rclone(),
        &[plain],
        &plan(1, None, None),
        None,
    )
    .unwrap();
    let remote = &report.remotes[0];
    assert!(!remote.ok);
    assert!(
        remote.error.as_deref().unwrap().contains("non-crypt"),
        "{remote:?}"
    );
    assert!(fixture.files_in_base(1).is_empty());
    assert!(report.leftovers.is_empty());
}

#[test]
#[ignore = "requires rclone"]
fn native_crypt_pool_writes_rclone_readable_files() {
    let fixture = Fixture::new(3);
    let remotes = remotes(3);
    let pool = pool_of(&remotes, true);
    let report = execute(
        &fixture.engine(true),
        &rclone(),
        &remotes,
        &plan(2, Some(3), Some(pool.workers)),
        Some(PoolTarget {
            name: "native",
            definition: &pool,
        }),
    )
    .unwrap();
    assert_ok(&report);
    assert_eq!(report.parallel, 3);
    assert!(report.estimate.is_some());
    assert_clean(&fixture, &report, &[1, 2, 3]);
}

#[test]
#[ignore = "requires rclone"]
fn one_large_file_streams() {
    let fixture = Fixture::new(1);
    let report = execute(
        &fixture.engine(false),
        &rclone(),
        &remotes(1),
        &plan(64, Some(1), None),
        None,
    )
    .unwrap();
    assert_ok(&report);
    assert_eq!((report.files_per_remote, report.parallel), (1, 1));
    assert_eq!(report.bottleneck_upload.as_deref(), Some("c1:pool"));
    assert_clean(&fixture, &report, &[1]);
}

#[test]
#[ignore = "requires rclone"]
fn many_small_files() {
    let fixture = Fixture::new(1);
    let report = execute(
        &fixture.engine(false),
        &rclone(),
        &remotes(1),
        &plan(1, Some(200), Some(8)),
        None,
    )
    .unwrap();
    assert_ok(&report);
    assert_eq!((report.files_per_remote, report.parallel), (200, 8));
    assert_clean(&fixture, &report, &[1]);
}

#[test]
#[ignore = "requires rclone"]
fn stop_during_upload_still_cleans_up() {
    let fixture = Fixture::new(2);
    let engine = fixture.engine(false);
    let cancel = engine.cancel.clone();
    let base = fixture.temp.path().join("base1");
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            let has_file = walk_has_file(&base);
            if has_file {
                cancel.store(true, Ordering::Release);
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    });
    let report = execute(
        &engine,
        &rclone(),
        &remotes(2),
        &plan(2, Some(256), Some(2)),
        None,
    )
    .unwrap();
    assert!(watcher.join().unwrap(), "upload never started");
    for remote in &report.remotes {
        assert!(!remote.ok, "{remote:?}");
        assert_eq!(remote.error.as_deref(), Some("cancelled"), "{remote:?}");
    }
    // The second remote was never touched.
    assert!(report.remotes[1].first_op_ms.is_none());
    assert_clean(&fixture, &report, &[1, 2]);
}

fn walk_has_file(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            walk_has_file(&path)
        } else {
            true
        }
    })
}
