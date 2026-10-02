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

/// Simultaneous transfer counts tried, in order (doubling up to 32).
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
/// Fewest shards queued for a level, even at level 1.
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
/// The recommendation reaches at least this share of the best rate. A cap
/// is an upper bound: erring a level high costs little, a level low
/// throttles small files.
const ENOUGH: f64 = 0.95;

/// Shards queued for a level: `FILES_PER_SLOT` per slot, at least `MIN_FILES`.
fn files_at(level: usize) -> usize {
    (level * FILES_PER_SLOT).max(MIN_FILES)
}

/// Progress bar share of one level (or of writing the read set). Tuning
/// moves an unknown number of bytes in a roughly fixed time, so the bar
/// advances per step, weighted like one remote's normal test (its upload
/// and read back) and reported as transferred: the bar then grows with
/// time and the GUI's rate-based remaining time stays meaningful.
pub(crate) fn step_bytes(plan: &TestPlan) -> u64 {
    (2 * plan.bytes_per_remote).max(1024 * 1024)
}

/// The bar share of one remote's tuning: one step per level (plus the read
/// set). Levels skipped by an early stop are settled by the caller.
pub(crate) fn expected_bytes(plan: &TestPlan) -> u64 {
    let levels = LEVELS.len() as u64;
    let uploads = if plan.tune_uploads { levels } else { 0 };
    let downloads = if plan.tune_downloads { levels + 1 } else { 0 };
    (uploads + downloads) * step_bytes(plan)
}

/// One step of the bar done.
fn step_done(run: &Run<'_>, outcome: &mut TuneOutcome) {
    let step = step_bytes(run.plan);
    crate::progress::advance(step, step);
    outcome.reported += step;
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
        if rate > 0.0 && rate >= best * GAIN {
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

/// Result of one remote's tuning, merged into its [`super::model::RemoteSpeed`].
pub(crate) struct TuneOutcome {
    /// Upload tuning, when `--tune-uploads` was given.
    pub uploads: Option<ConcurrencyTuning>,
    /// Download tuning, when `--tune-downloads` was given.
    pub downloads: Option<ConcurrencyTuning>,
    /// Bytes reported to the progress bar.
    pub reported: u64,
    /// Level folders that may be left behind.
    pub leftover: Vec<String>,
}

/// Shards uploaded once and read back at every download level.
/// Twice the highest level, so no shard is read by two slots at once and
/// each read within a level is mostly of a different object.
const READ_SET: usize = 64;

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
    /// One-line error of a failed level.
    error: Option<String>,
}

/// Sets a flag when dropped, also while a panic unwinds (the level's
/// watcher must always end).
struct SetOnDrop<'a>(&'a AtomicBool);
impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Fewest shards whose duration rates a level by their mean.
const MIN_SAMPLES: usize = 3;

/// Rate of `level` slots from the shards that started in `[from, until]`:
/// with every slot busy, throughput is `level × shard / mean duration`
/// (Little's law). Unlike counting finished shards in the window, this does
/// not jump by a whole shard (about ±1/n of n shards) at low levels, and a
/// shard still in flight when the window ends counts once it finishes.
pub(crate) fn rate_from_durations(
    level: usize,
    shard_bytes: u64,
    finished: &[(Instant, Instant)],
    from: Instant,
    until: Option<Instant>,
    min_samples: usize,
) -> Option<f64> {
    let samples: Vec<f64> = finished
        .iter()
        .filter(|(began, _)| *began >= from && until.is_none_or(|end| *began <= end))
        .map(|(began, ended)| ended.duration_since(*began).as_secs_f64())
        .collect();
    if samples.is_empty() || samples.len() < min_samples {
        return None;
    }
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    Some(level as f64 * shard_bytes as f64 / mean.max(1e-3))
}

/// Runs one level with both process caps lifted (and the account's requests
/// per second split as a cap of `level` would split them). Shards started
/// in the first [`WARM_UP`] (process start, TLS, the provider's first
/// answer) are not rated; after [`WINDOW`] more no further shard starts, but
/// the ones in flight finish, so every started upload also commits (a
/// provider that rejects concurrent commits, e.g. Dropbox, shows as an
/// error). The rate comes from the durations of the shards started in the
/// window ([`rate_from_durations`]); with too few of them (shards slower
/// than the window), from every shard that finished.
fn timed<T: Send>(
    run: &Run<'_>,
    level: usize,
    files: &[TestFile],
    go: impl FnOnce(&super::engine::Engine, &Transfer) -> Phase<T>,
) -> Measured {
    let stop = Arc::new(AtomicBool::new(false));
    let drain = Arc::new(AtomicBool::new(false));
    let engine = run.engine.scoped(stop.clone(), drain.clone());
    let progress = Transfer::silent();
    let finished = AtomicBool::new(false);
    let started = Instant::now();
    let (phase, drained_at) = std::thread::scope(|scope| {
        let watcher = scope.spawn(|| {
            let mut drained_at = None;
            while !finished.load(Ordering::Acquire) {
                if run.engine.cancelled() {
                    stop.store(true, Ordering::Release);
                }
                if drained_at.is_none() && started.elapsed() >= WARM_UP + WINDOW {
                    drain.store(true, Ordering::Release);
                    drained_at = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drained_at
        });
        let phase = {
            let _done = SetOnDrop(&finished);
            let _uncapped = crate::storage::rclone::uncap_for_speed_test(level);
            go(&engine, &progress)
        };
        (phase, watcher.join().unwrap_or(None))
    });
    let error = if run.engine.cancelled() {
        Some("cancelled".to_owned())
    } else {
        phase.error().map(str::to_owned)
    };
    let shard = files.first().map_or(0, |f| f.size);
    let durations = progress
        .finished
        .lock()
        .map(|f| f.clone())
        .unwrap_or_default();
    let rate = match error {
        Some(_) => None,
        None => rate_from_durations(
            level,
            shard,
            &durations,
            started + WARM_UP,
            drained_at,
            MIN_SAMPLES,
        )
        // Shards slower than the window: every finished one, warm-up included.
        .or_else(|| rate_from_durations(level, shard, &durations, started, None, 1))
        .or_else(|| {
            phase
                .complete()
                .then(|| rate(files.iter().map(|f| f.size).sum(), phase.wall))
        }),
    };
    Measured { rate, error }
}

/// [`timed`], once more after a failure that may be transient (a reset
/// connection, a 5xx): a level fails only when it fails twice.
fn timed_twice<T: Send>(
    run: &Run<'_>,
    level: usize,
    files: &[TestFile],
    go: impl Fn(&super::engine::Engine, &Transfer) -> Phase<T>,
) -> Measured {
    let first = timed(run, level, files, &go);
    if first.error.is_none() || run.engine.cancelled() {
        return first;
    }
    timed(run, level, files, &go)
}

/// Removes a level's shards, up to three passes: a tuning level can leave
/// hundreds of small shards, and on an account that deletes one at a time
/// (Dropbox) some deletes may time out waiting for their turn.
fn clean(
    engine: &super::engine::Engine,
    files: &[TestFile],
    attempted: &[AtomicBool],
    parallel: usize,
    dir: &str,
    test_dir: &str,
) -> bool {
    (0..3).any(|_| cleanup::remove_run(engine, files, attempted, parallel, dir, test_dir))
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

/// `cancelled` after a stop, else the given error.
fn cancelled_or(engine: &super::engine::Engine, error: Option<&str>) -> Option<String> {
    if engine.cancelled() {
        Some("cancelled".to_owned())
    } else {
        error.map(str::to_owned)
    }
}

/// Bytes per second over `wall` (at least 1 ms to avoid division by zero).
fn rate(bytes: u64, wall: std::time::Duration) -> f64 {
    bytes as f64 / wall.as_secs_f64().max(1e-3)
}

/// Runs upload levels 1, 2, 4, … in their own folders until climbing stops,
/// recording each step and the recommendation in `tuning`.
fn upload_levels(
    run: &Run<'_>,
    at: &Position<'_>,
    test_dir: &str,
    tuning: &mut ConcurrencyTuning,
    outcome: &mut TuneOutcome,
) {
    let engine = run.engine;
    for (step, level) in LEVELS.into_iter().enumerate() {
        if engine.cancelled() || !climb_further(&tuning.steps) {
            break;
        }
        let downloads_after = if run.plan.tune_downloads {
            super::remaining::READ_SET_MAX + super::remaining::levels(LEVELS.len())
        } else {
            Duration::ZERO
        };
        super::remaining::say(
            at,
            run.plan,
            &run.budget,
            super::remaining::levels(LEVELS.len() - step) + downloads_after,
        );
        at.status(&format!("tuning uploads: {level} at once"));
        let dir = remote_join(test_dir, &format!("{}-tune-{level:02}", run.run_id));
        let files = shard_files(run, &dir, files_at(level));
        let attempted: Vec<AtomicBool> = files.iter().map(|_| AtomicBool::new(false)).collect();
        // Distinct contents per level: no provider-side dedupe.
        let seed = level_seed(&run.seed, &format!("tune-{level}"));
        let measured = timed_twice(run, level, &files, |engine, progress| {
            transfer::upload_into(engine, at, &files, level, &seed, &attempted, progress)
        });
        step_done(run, outcome);
        tuning.steps.push(TuningStep {
            parallel: level,
            files: files.len(),
            bytes_per_s: measured.rate,
            error: measured.error,
        });
        at.status("cleaning up");
        if !clean(engine, &files, &attempted, level, &dir, test_dir) {
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
    super::remaining::say(
        at,
        run.plan,
        &run.budget,
        super::remaining::READ_SET_MAX + super::remaining::levels(LEVELS.len()),
    );
    at.status("tuning downloads: writing the read set");
    // Setup, not a measurement: written under the account's normal caps
    // (Dropbox: one at a time).
    let written = {
        transfer::upload_into(
            engine,
            at,
            &set,
            16,
            &level_seed(&run.seed, "tune-read"),
            &attempted,
            &Transfer::silent(),
        )
    };
    step_done(run, outcome);
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
        for (step, level) in LEVELS.into_iter().enumerate() {
            if engine.cancelled() || !climb_further(&tuning.steps) {
                break;
            }
            super::remaining::say(
                at,
                run.plan,
                &run.budget,
                super::remaining::levels(LEVELS.len() - step),
            );
            at.status(&format!("tuning downloads: {level} at once"));
            let count = files_at(level);
            let reads: Vec<TestFile> = (0..count)
                .map(|i| TestFile {
                    address: set[i % READ_SET].address.clone(),
                    size: set[i % READ_SET].size,
                })
                .collect();
            let expected: Vec<blake3::Hash> = (0..count).map(|i| digests[i % READ_SET]).collect();
            let measured = timed_twice(run, level, &reads, |engine, progress| {
                transfer::download_into(engine, at, &reads, level, &expected, progress)
            });
            step_done(run, outcome);
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
    if !clean(engine, &set, &attempted, 16, &dir, test_dir) {
        outcome.leftover.push(dir);
    }
}

/// Per-level data key derived from the run seed and `label`, so levels never
/// reuse the same stream.
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
        // 7.5 < 0.95 * 8.1 <= 8.0.
        assert_eq!(recommend(&steps), Some(8));
        // Errors never count, the counts before them do.
        let limited = [
            step(1, Some(3.0), None),
            step(2, None, Some("rate limited")),
        ];
        assert_eq!(recommend(&limited), Some(1));
    }

    #[test]
    fn rate_follows_little_law_over_the_window() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        // 4 slots, 1 MB shards of 2 s each: 2 MB/s.
        let finished: Vec<_> = (0..12)
            .map(|i| {
                let began = at(f64::from(i / 4) * 2.0);
                (began, began + Duration::from_secs(2))
            })
            .collect();
        let rate =
            rate_from_durations(4, 1_000_000, &finished, at(0.0), None, MIN_SAMPLES).unwrap();
        assert!((rate - 2_000_000.0).abs() < 1.0, "{rate}");
        // Shards started before `from` or after `until` do not count.
        assert_eq!(
            rate_from_durations(4, 1_000_000, &finished, at(3.0), Some(at(3.5)), MIN_SAMPLES),
            None,
            "too few samples"
        );
        assert!(
            rate_from_durations(4, 1_000_000, &finished, at(2.0), Some(at(4.0)), MIN_SAMPLES)
                .is_some()
        );
    }

    #[test]
    fn zero_rates_never_count_as_a_gain() {
        let zeros = [step(1, Some(0.0), None), step(2, Some(0.0), None)];
        assert!(!climb_further(&zeros));
        assert_eq!(recommend(&zeros), None);
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
        // One bar step per level, weighted like the normal test (2 x 1 MiB).
        assert_eq!(expected_bytes(&plan), 6 * 2 * MIB);
        plan.tune_downloads = true;
        // Plus the download levels and the read set.
        assert_eq!(expected_bytes(&plan), 13 * 2 * MIB);
    }
}
