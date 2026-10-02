//! `--tune-uploads` / `--tune-downloads`: the best number of simultaneous
//! shard uploads / downloads per account.
//!
//! The unit is one small shard ([`TUNE_SHARD_BYTES`], or the pool's shard
//! size when smaller), one request each (no chunk fan-out in rclone). The
//! count a remote needs is largest for small shards, where the per-request
//! overhead dominates; it also suits large shards, which a lower count
//! already fills with bandwidth (and a higher cap costs them nothing).
//!
//! After a remote passed the normal test, such shards are uploaded at 1, 2,
//! 4, … [`LEVELS`] at once, each level in its own folder that is deleted
//! right after. Downloads first upload a read set of [`READ_SET`] shards and
//! read it (cycling, verified) at the same levels. Each level is time-boxed
//! (see [`timed`]). Both process caps are lifted meanwhile, so the account's
//! own behavior shows: throttling as a rate that stops rising, a provider
//! that refuses concurrent writes as an error.
//!
//! Climbing stops at the first error, or after two levels in a row that
//! are not [`GAIN`] faster than the best so far. The recommendation is the
//! smallest tested count within [`ENOUGH`] of the best rate: more uploads
//! than that only add load (and risk throttling) for little gain.
use super::cleanup;
use super::model::{ConcurrencyTuning, TuningStep};
use super::options::TestPlan;
use super::progress::Position;
use super::progress::Transfer;
use super::remote::Run;
use super::transfer::Phase;
use super::transfer::{self, TestFile};
use crate::utils::remote_join;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) const LEVELS: [usize; 6] = [1, 2, 4, 8, 16, 32];
/// Size of one tuning request (when the pool's shards are not smaller).
pub(crate) const TUNE_SHARD_BYTES: u64 = 1024 * 1024;
/// Shards queued per slot: enough that a fast account still fills the
/// measuring window; the ones not started when it ends are never written.
#[cfg(not(test))]
const FILES_PER_SLOT: usize = 64;
/// Local test remotes are fast: keep the bytes a window writes small.
#[cfg(test)]
const FILES_PER_SLOT: usize = 3;
const MIN_FILES: usize = 2;
/// Discarded start of every level: process start, TLS, the provider's
/// first answer.
#[cfg(not(test))]
pub(crate) const WARM_UP: Duration = Duration::from_secs(5);
#[cfg(test)]
pub(crate) const WARM_UP: Duration = Duration::from_millis(300);
/// Measured part of every level; the level then stops.
#[cfg(not(test))]
pub(crate) const WINDOW: Duration = Duration::from_secs(15);
#[cfg(test)]
pub(crate) const WINDOW: Duration = Duration::from_millis(900);
/// A level must be this much faster than the best so far to count as a gain.
const GAIN: f64 = 1.10;
/// Levels without gain after which climbing stops.
const FLAT_LEVELS: usize = 2;
/// The recommendation reaches at least this share of the best rate.
const ENOUGH: f64 = 0.90;

fn files_at(level: usize) -> usize {
    (level * FILES_PER_SLOT).max(MIN_FILES)
}

/// Progress bar share of one level (or of writing the read set): tuning
/// moves an unknown number of bytes, so the bar advances per step instead.
pub(crate) const STEP_BYTES: u64 = 16 * 1024 * 1024;

/// The bar share of one remote's tuning: one step per level (plus the read
/// set). Levels skipped by an early stop are settled by the caller.
pub(crate) fn expected_bytes(plan: &TestPlan) -> u64 {
    let levels = LEVELS.len() as u64;
    let uploads = if plan.tune_uploads { levels } else { 0 };
    let downloads = if plan.tune_downloads { levels + 1 } else { 0 };
    (uploads + downloads) * STEP_BYTES
}

/// One step of the bar done.
fn step_done(outcome: &mut TuneOutcome) {
    super::progress::settle(STEP_BYTES, 0);
    outcome.reported += STEP_BYTES;
}

/// Whether another level should run after `steps`.
pub(crate) fn climb_further(steps: &[TuningStep]) -> bool {
    let Some(last) = steps.last() else {
        return true;
    };
    if last.error.is_some() || last.bytes_per_s.is_none() {
        return false;
    }
    let mut best = 0.0f64;
    let mut flat = 0;
    for rate in steps.iter().filter_map(|s| s.bytes_per_s) {
        if rate >= best * GAIN {
            flat = 0;
        } else {
            flat += 1;
        }
        best = best.max(rate);
    }
    flat < FLAT_LEVELS
}

/// Smallest tested count within [`ENOUGH`] of the best successful rate.
pub(crate) fn recommend(steps: &[TuningStep]) -> Option<usize> {
    let ok = || {
        steps
            .iter()
            .filter(|s| s.error.is_none())
            .filter_map(|s| s.bytes_per_s.map(|rate| (s.parallel, rate)))
            .filter(|(_, rate)| rate.is_finite() && *rate > 0.0)
    };
    let best = ok().map(|(_, rate)| rate).fold(0.0f64, f64::max);
    ok().filter(|(_, rate)| *rate >= best * ENOUGH)
        .map(|(parallel, _)| parallel)
        .min()
}

pub(crate) struct TuneOutcome {
    pub uploads: Option<ConcurrencyTuning>,
    pub downloads: Option<ConcurrencyTuning>,
    /// Bytes reported to the progress bar.
    pub reported: u64,
    /// Level folders that may be left behind.
    pub leftover: Vec<String>,
}

/// Shards uploaded once and read back at every download level.
const READ_SET: usize = 16;

/// Runs the requested tunings on one remote; never fails the remote.
pub(crate) fn run(
    run: &Run<'_>,
    at: &Position<'_>,
    test_dir: &str,
    account: Option<String>,
) -> TuneOutcome {
    let mut outcome = TuneOutcome {
        uploads: None,
        downloads: None,
        reported: 0,
        leftover: Vec::new(),
    };
    let settings = crate::storage::account::runtime::settings();
    let own = |pick: fn(&crate::storage::account::runtime::Settings, &str) -> Option<usize>| {
        account.as_deref().and_then(|a| pick(&settings, a))
    };
    if run.plan.tune_uploads {
        let mut tuning = ConcurrencyTuning::new(account.clone(), run.plan.shard_bytes);
        tuning.current = own(crate::storage::account::runtime::Settings::max_uploads);
        upload_levels(run, at, test_dir, &mut tuning, &mut outcome);
        outcome.uploads = Some(tuning);
    }
    if run.plan.tune_downloads && !run.engine.cancelled() {
        let mut tuning = ConcurrencyTuning::new(account.clone(), run.plan.shard_bytes);
        tuning.current = own(crate::storage::account::runtime::Settings::max_downloads);
        download_levels(run, at, test_dir, &mut tuning, &mut outcome);
        outcome.downloads = Some(tuning);
    }
    outcome
}

/// One measured level.
struct Measured {
    /// None when the level failed.
    rate: Option<f64>,
    error: Option<String>,
}

/// Runs one level with both process caps lifted: after [`WARM_UP`] the
/// bytes moved during [`WINDOW`] give the rate. Then no further file starts
/// but the ones in flight finish, so every started upload also commits (a
/// provider that rejects concurrent commits, e.g. Dropbox, shows as an error
/// even with shards too large for the window). A level that finishes
/// earlier is rated on all of its bytes.
fn timed<T: Send>(
    run: &Run<'_>,
    files: &[TestFile],
    go: impl FnOnce(&super::engine::Engine, &Transfer) -> Phase<T>,
) -> Measured {
    let stop = Arc::new(AtomicBool::new(false));
    let drain = Arc::new(AtomicBool::new(false));
    let engine = run.engine.scoped(stop.clone(), drain.clone());
    let progress = Transfer::silent();
    let finished = AtomicBool::new(false);
    let started = Instant::now();
    let moved = || progress.moved.load(Ordering::Relaxed);
    let (phase, window) = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            let mut warm = None;
            let mut window = None;
            while !finished.load(Ordering::Acquire) {
                if run.engine.cancelled() {
                    stop.store(true, Ordering::Release);
                }
                let at = started.elapsed();
                if warm.is_none() && at >= WARM_UP {
                    warm = Some((at, moved()));
                }
                if window.is_none() && at >= WARM_UP + WINDOW {
                    drain.store(true, Ordering::Release);
                    window = warm.map(|start| (start, (at, moved())));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            window
        });
        let phase = {
            let _uncapped = crate::storage::rclone::uncap_for_speed_test();
            go(&engine, &progress)
        };
        finished.store(true, Ordering::Release);
        (phase, watcher.join().unwrap_or(None))
    });
    let error = if run.engine.cancelled() {
        Some("cancelled".to_owned())
    } else {
        phase.error().map(str::to_owned)
    };
    let rate = match (&error, window) {
        (Some(_), _) => None,
        (None, Some(((t0, b0), (t1, b1)))) => Some(rate(b1.saturating_sub(b0), t1 - t0)),
        (None, None) if phase.complete() => {
            Some(rate(files.iter().map(|f| f.size).sum(), phase.wall))
        }
        (None, None) => None,
    };
    Measured { rate, error }
}

/// `count` shard files under `dir` (named by index).
fn shard_files(run: &Run<'_>, dir: &str, count: usize) -> Vec<TestFile> {
    (0..count)
        .map(|i| TestFile {
            address: remote_join(dir, &format!("{i:04}.bin")),
            size: run.plan.shard_bytes,
        })
        .collect()
}

fn cancelled_or(engine: &super::engine::Engine, error: Option<&str>) -> Option<String> {
    if engine.cancelled() {
        Some("cancelled".to_owned())
    } else {
        error.map(str::to_owned)
    }
}

fn rate(bytes: u64, wall: std::time::Duration) -> f64 {
    bytes as f64 / wall.as_secs_f64().max(1e-3)
}

fn upload_levels(
    run: &Run<'_>,
    at: &Position<'_>,
    test_dir: &str,
    tuning: &mut ConcurrencyTuning,
    outcome: &mut TuneOutcome,
) {
    let engine = run.engine;
    for level in LEVELS {
        if engine.cancelled() || !climb_further(&tuning.steps) {
            break;
        }
        at.status(&format!("tuning uploads: {level} at once"));
        let dir = remote_join(test_dir, &format!("{}-tune-{level:02}", run.run_id));
        let files = shard_files(run, &dir, files_at(level));
        let attempted: Vec<AtomicBool> = files.iter().map(|_| AtomicBool::new(false)).collect();
        // Distinct contents per level: no provider-side dedupe.
        let seed = level_seed(&run.seed, &format!("tune-{level}"));
        let measured = timed(run, &files, |engine, progress| {
            transfer::upload_into(engine, at, &files, level, &seed, &attempted, progress)
        });
        step_done(outcome);
        tuning.steps.push(TuningStep {
            parallel: level,
            files: files.len(),
            bytes_per_s: measured.rate,
            error: measured.error,
        });
        at.status("cleaning up");
        if !cleanup::remove_run(engine, &files, &attempted, level, &dir, test_dir) {
            outcome.leftover.push(dir);
        }
    }
    tuning.recommended = recommend(&tuning.steps);
}

/// Uploads [`READ_SET`] shards, then reads them (cycling) at 1, 2, 4, …
/// at once with every cap lifted, verifying each read.
fn download_levels(
    run: &Run<'_>,
    at: &Position<'_>,
    test_dir: &str,
    tuning: &mut ConcurrencyTuning,
    outcome: &mut TuneOutcome,
) {
    let engine = run.engine;
    let dir = remote_join(test_dir, &format!("{}-tune-read", run.run_id));
    let set = shard_files(run, &dir, READ_SET);
    let attempted: Vec<AtomicBool> = set.iter().map(|_| AtomicBool::new(false)).collect();
    at.status("tuning downloads: writing the read set");
    let written = {
        let _uncapped = crate::storage::rclone::uncap_for_speed_test();
        transfer::upload_into(
            engine,
            at,
            &set,
            READ_SET,
            &level_seed(&run.seed, "tune-read"),
            &attempted,
            &Transfer::silent(),
        )
    };
    step_done(outcome);
    if let Some(error) = cancelled_or(engine, written.error()).or_else(|| {
        (!written.complete()).then(|| "not every read-set shard was written".to_owned())
    }) {
        tuning.error = Some(format!("read set: {error}"));
    } else {
        let digests: Vec<blake3::Hash> = written
            .results
            .into_iter()
            .filter_map(|r| r.and_then(Result::ok))
            .collect();
        for level in LEVELS {
            if engine.cancelled() || !climb_further(&tuning.steps) {
                break;
            }
            at.status(&format!("tuning downloads: {level} at once"));
            let count = files_at(level);
            let reads: Vec<TestFile> = (0..count)
                .map(|i| TestFile {
                    address: set[i % READ_SET].address.clone(),
                    size: set[i % READ_SET].size,
                })
                .collect();
            let expected: Vec<blake3::Hash> = (0..count).map(|i| digests[i % READ_SET]).collect();
            let measured = timed(run, &reads, |engine, progress| {
                transfer::download_into(engine, at, &reads, level, &expected, progress)
            });
            step_done(outcome);
            tuning.steps.push(TuningStep {
                parallel: level,
                files: count,
                bytes_per_s: measured.rate,
                error: measured.error,
            });
        }
        tuning.recommended = recommend(&tuning.steps);
    }
    at.status("cleaning up");
    if !cleanup::remove_run(engine, &set, &attempted, READ_SET, &dir, test_dir) {
        outcome.leftover.push(dir);
    }
}

fn level_seed(seed: &[u8; 32], label: &str) -> [u8; 32] {
    *blake3::keyed_hash(seed, label.as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(parallel: usize, mb_s: Option<f64>, error: Option<&str>) -> TuningStep {
        TuningStep {
            parallel,
            files: files_at(parallel),
            bytes_per_s: mb_s.map(|v| v * 1e6),
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn climbing_stops_on_error_or_two_flat_levels() {
        assert!(climb_further(&[]));
        assert!(climb_further(&[step(1, Some(2.0), None)]));
        assert!(!climb_further(&[step(1, None, Some("429"))]));
        let rising = [
            step(1, Some(2.0), None),
            step(2, Some(4.0), None),
            step(4, Some(7.0), None),
        ];
        assert!(climb_further(&rising));
        // One flat level is noise; two end the climb.
        let one_flat = [step(1, Some(2.0), None), step(2, Some(2.1), None)];
        assert!(climb_further(&one_flat));
        let two_flat = [
            step(1, Some(2.0), None),
            step(2, Some(2.1), None),
            step(4, Some(1.9), None),
        ];
        assert!(!climb_further(&two_flat));
        // A gain in between resets the count.
        let reset = [
            step(1, Some(2.0), None),
            step(2, Some(2.1), None),
            step(4, Some(5.0), None),
            step(8, Some(5.1), None),
        ];
        assert!(climb_further(&reset));
        let failed = [
            step(1, Some(2.0), None),
            step(2, None, Some("too_many_write_operations")),
        ];
        assert!(!climb_further(&failed));
    }

    #[test]
    fn recommendation_is_the_smallest_count_near_the_best() {
        assert_eq!(recommend(&[]), None);
        assert_eq!(recommend(&[step(1, None, Some("x"))]), None);
        let steps = [
            step(1, Some(2.0), None),
            step(2, Some(4.0), None),
            step(4, Some(7.5), None),
            step(8, Some(8.0), None),
            step(16, Some(8.1), None),
        ];
        // 7.5 >= 0.9 * 8.1.
        assert_eq!(recommend(&steps), Some(4));
        // Errors never count, the counts before them do.
        let limited = [
            step(1, Some(3.0), None),
            step(2, None, Some("rate limited")),
        ];
        assert_eq!(recommend(&limited), Some(1));
    }

    #[test]
    fn expected_bytes_cover_every_level() {
        const MIB: u64 = 1024 * 1024;
        let mut plan = TestPlan {
            bytes_per_remote: MIB,
            file_sizes: vec![MIB],
            parallel: 1,
            tune_uploads: false,
            tune_downloads: false,
            shard_bytes: MIB,
        };
        assert_eq!(expected_bytes(&plan), 0);
        plan.tune_uploads = true;
        // One bar step per level.
        assert_eq!(expected_bytes(&plan), 6 * STEP_BYTES);
        plan.tune_downloads = true;
        // Plus the download levels and the read set.
        assert_eq!(expected_bytes(&plan), 13 * STEP_BYTES);
    }
}
