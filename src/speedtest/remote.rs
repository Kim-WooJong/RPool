//! The test of one remote: first operation, latency, free space, upload,
//! read back + verify, cleanup.
use super::cleanup;
use super::engine::{one_line, Engine};
use super::model::{RemoteSpeed, TEST_DIR};
use super::options::TestPlan;
use super::progress::{self, Position};
use super::transfer::{self, TestFile};
use crate::storage::admin::{BackendAdmin, RcloneAdmin};
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::utils::remote_join;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Further small operations whose median is `latency_ms`.
const LATENCY_SAMPLES: usize = 3;
const SMALL_OP: Duration = Duration::from_secs(120);

/// Result of one remote: its report row and the test folders where
/// something may be left.
pub(crate) struct RemoteOutcome {
    pub speed: RemoteSpeed,
    pub leftover: Vec<String>,
}

/// Inputs shared by every remote of one run.
pub(crate) struct Run<'a> {
    pub engine: &'a Engine,
    pub plan: &'a TestPlan,
    pub seed: [u8; 32],
    pub run_id: String,
    pub config: Option<&'a serde_json::Value>,
}

/// Bottom backend type behind `remote` (crypt → its base → … → `type`).
pub(crate) fn backend_type(config: &serde_json::Value, remote: &str) -> Option<String> {
    let name = crate::storage::rclone::remote_name(remote).ok()?;
    config.get(name)?;
    let (bottom, _) = crate::storage::rclone::write_base(config, name);
    config
        .get(&bottom)?
        .get("type")?
        .as_str()
        .map(str::to_ascii_lowercase)
}

/// A stat of an address that should not exist: NotFound is the expected,
/// successful answer.
fn probe(run: &Run<'_>, address: &str) -> Result<Duration, StorageError> {
    let started = Instant::now();
    match run
        .engine
        .context
        .stat_raw(&run.engine.step(SMALL_OP), address)
    {
        Ok(_) => Ok(started.elapsed()),
        Err(error) if error.kind() == StorageErrorKind::NotFound => Ok(started.elapsed()),
        Err(error) => Err(error),
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

/// Bytes the encrypted files need on the remote: rclone crypt adds a 32-byte
/// header and 16 bytes per 64 KiB block.
fn stored_bytes(plan: &TestPlan) -> u64 {
    plan.file_sizes
        .iter()
        .map(|size| size + 32 + size.div_ceil(64 * 1024) * 16)
        .sum()
}

/// Best effort: refuses only when the remote reports less free space.
fn check_free_space(run: &Run<'_>, root: &str) -> Result<(), String> {
    let report = RcloneAdmin::new(run.engine.context.clone()).quota(root);
    let needed = stored_bytes(run.plan);
    match report.free {
        Some(free) if free < needed => Err(format!(
            "not enough free space: the test needs {:.1} MiB, the remote reports {:.1} MiB free",
            needed as f64 / 1048576.0,
            free as f64 / 1048576.0
        )),
        _ => Ok(()),
    }
}

/// Tests one remote; never fails the whole run.
pub(crate) fn test(run: &Run<'_>, at: &Position<'_>) -> RemoteOutcome {
    let mut speed = RemoteSpeed {
        remote: at.remote.to_owned(),
        backend: run.config.and_then(|c| backend_type(c, at.remote)),
        ok: false,
        error: None,
        first_op_ms: None,
        latency_ms: None,
        upload_bytes_per_s: None,
        download_bytes_per_s: None,
        upload_seconds: None,
        download_seconds: None,
        verified: false,
        upload_tuning: None,
    };
    let root = match crate::remote_root::apply_remote_root(at.remote) {
        Ok(root) => root,
        Err(error) => {
            speed.error = Some(one_line(&format!("remote root: {error:#}")));
            // This remote's share of the bar, tuning included.
            progress::settle(
                2 * run.plan.bytes_per_remote + super::tune::expected_bytes(run.plan),
                0,
            );
            return RemoteOutcome {
                speed,
                leftover: Vec::new(),
            };
        }
    };
    let test_dir = remote_join(&root, TEST_DIR);
    let run_dir = remote_join(&test_dir, &run.run_id);
    let files: Vec<TestFile> = run
        .plan
        .file_sizes
        .iter()
        .enumerate()
        .map(|(i, &size)| TestFile {
            address: remote_join(&run_dir, &format!("{i:04}.bin")),
            size,
        })
        .collect();
    let attempted: Vec<AtomicBool> = files.iter().map(|_| AtomicBool::new(false)).collect();

    let mut reported = 0u64;
    let error = measure(
        run,
        at,
        &root,
        &run_dir,
        &files,
        &attempted,
        &mut speed,
        &mut reported,
    )
    .err();
    // Upload + read back of this remote, whatever did not move.
    progress::settle(2 * run.plan.bytes_per_remote, reported);
    speed.ok = error.is_none();
    speed.error = error;

    let mut leftover = Vec::new();
    if attempted
        .iter()
        .any(|a| a.load(std::sync::atomic::Ordering::Acquire))
    {
        at.status("cleaning up");
        if !cleanup::remove_run(
            run.engine,
            &files,
            &attempted,
            run.plan.parallel,
            &run_dir,
            &test_dir,
        ) {
            leftover.push(run_dir);
        }
    }
    let mut tuned = 0;
    if run.plan.tune_uploads && speed.ok && !run.engine.cancelled() {
        let account = run.config.and_then(|config| {
            let name = crate::storage::rclone::remote_name(at.remote).ok()?;
            config.get(name)?;
            Some(crate::storage::rclone::write_base(config, name).0)
        });
        let outcome = super::tune::run(run, at, &test_dir, account);
        tuned = outcome.reported;
        leftover.extend(outcome.leftover);
        speed.upload_tuning = Some(outcome.tuning);
    }
    progress::settle(super::tune::expected_bytes(run.plan), tuned);
    at.done(speed.error.as_deref());
    RemoteOutcome { speed, leftover }
}

fn stop_check(engine: &Engine) -> Result<(), String> {
    if engine.cancelled() {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn op_error(engine: &Engine, what: &str, error: StorageError) -> String {
    if engine.cancelled() {
        "cancelled".into()
    } else {
        one_line(&format!("{what}: {error}"))
    }
}

/// Steps until the first error (the error is the report's one line).
/// `reported` collects the bytes reported as moved.
#[allow(clippy::too_many_arguments)]
fn measure(
    run: &Run<'_>,
    at: &Position<'_>,
    root: &str,
    run_dir: &str,
    files: &[TestFile],
    attempted: &[AtomicBool],
    speed: &mut RemoteSpeed,
    reported: &mut u64,
) -> Result<(), String> {
    let engine = run.engine;
    stop_check(engine)?;
    at.status("first operation");
    let first = probe(run, run_dir).map_err(|e| op_error(engine, "first operation", e))?;
    speed.first_op_ms = Some(millis(first));

    at.status("latency");
    let mut samples = Vec::with_capacity(LATENCY_SAMPLES);
    for _ in 0..LATENCY_SAMPLES {
        stop_check(engine)?;
        let sample = probe(run, run_dir).map_err(|e| op_error(engine, "latency", e))?;
        samples.push(millis(sample));
    }
    speed.latency_ms = median(samples);

    stop_check(engine)?;
    at.status("checking free space");
    check_free_space(run, root)?;

    stop_check(engine)?;
    let plan = run.plan;
    let (upload, progress) =
        transfer::upload(engine, at, files, plan.parallel, &run.seed, attempted);
    *reported += progress.reported();
    if let Some(error) = upload.error() {
        return Err(error.to_owned());
    }
    stop_check(engine)?;
    if !upload.complete() {
        return Err("upload: not every file was written".into());
    }
    let seconds = upload.wall.as_secs_f64().max(1e-3);
    speed.upload_seconds = Some(seconds);
    speed.upload_bytes_per_s = Some(plan.bytes_per_remote as f64 / seconds);
    let digests: Vec<blake3::Hash> = upload
        .results
        .into_iter()
        .filter_map(|r| r.and_then(Result::ok))
        .collect();

    // Some providers pay a long cold start on the first read of a process
    // (Drime: ~30 s), not on metadata calls. Time it on its own so the
    // download rate below is the sustained one.
    stop_check(engine)?;
    at.status("first read");
    let started = Instant::now();
    let first_byte = crate::storage::traits::ReadRange::new(0, 1)
        .map_err(|e| op_error(engine, "first read", e))?;
    engine
        .context
        .read_raw(
            &engine.step(SMALL_OP),
            &files[0].address,
            Some(&first_byte),
            &mut std::io::sink(),
        )
        .map_err(|e| op_error(engine, "first read", e))?;
    let first_read = millis(started.elapsed());
    speed.first_op_ms = speed.first_op_ms.max(Some(first_read));

    let (download, progress) = transfer::download(engine, at, files, plan.parallel, &digests);
    *reported += progress.reported();
    if let Some(error) = download.error() {
        return Err(error.to_owned());
    }
    stop_check(engine)?;
    if !download.complete() {
        return Err("download: not every file was read".into());
    }
    let seconds = download.wall.as_secs_f64().max(1e-3);
    speed.download_seconds = Some(seconds);
    speed.download_bytes_per_s = Some(plan.bytes_per_remote as f64 / seconds);
    speed.verified = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn backend_type_follows_crypt_to_its_base() {
        let config = json!({
            "c1": {"type": "crypt", "remote": "d1:rpool"},
            "d1": {"type": "Dropbox"},
            "c2": {"type": "crypt", "remote": "al:x"},
            "al": {"type": "alias", "remote": "sf:/data"},
            "sf": {"type": "sftp"},
            "plain": {"type": "local"},
            "dangling": {"type": "crypt", "remote": "missing:x"},
        });
        assert_eq!(
            backend_type(&config, "c1:rpool").as_deref(),
            Some("dropbox")
        );
        assert_eq!(backend_type(&config, "c2:").as_deref(), Some("sftp"));
        assert_eq!(backend_type(&config, "plain:").as_deref(), Some("local"));
        assert_eq!(
            backend_type(&config, "dangling:"),
            None,
            "base not configured"
        );
        assert_eq!(backend_type(&config, "nope:"), None);
        assert_eq!(backend_type(&config, "no-colon"), None);
    }

    #[test]
    fn median_and_crypt_overhead() {
        assert_eq!(median(vec![30, 10, 20]), Some(20));
        assert_eq!(median(vec![]), None);
        let plan = TestPlan {
            bytes_per_remote: 2 * 65536,
            file_sizes: vec![65536, 65536],
            parallel: 2,
            tune_uploads: false,
            shard_bytes: 64 * 65536,
        };
        assert_eq!(stored_bytes(&plan), 2 * (65536 + 32 + 16));
    }
}
