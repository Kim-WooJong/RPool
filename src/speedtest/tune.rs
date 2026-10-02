//! `--tune-uploads`: the best number of simultaneous uploads per account.
//!
//! After a remote passed the normal test, 1 MiB files are uploaded at 1, 2,
//! 4, … [`LEVELS`] at once (two files per upload slot, at least four), each
//! level in its own folder that is deleted right after. Both process caps
//! are lifted meanwhile, so the account's own behavior shows: rclone retries
//! throttled calls (HTTP 429) itself, which shows as a rate that stops
//! rising; a provider that refuses concurrent writes shows as an error.
//! Climbing stops at the first error, or after two levels in a row that
//! are not [`GAIN`] faster than the best so far. The recommendation is the
//! smallest tested count within [`ENOUGH`] of the best rate: more uploads
//! than that only add load (and risk throttling) for little gain.
use super::cleanup;
use super::model::{TuningStep, UploadTuning};
use super::options::{TestPlan, MIB};
use super::progress::Position;
use super::remote::Run;
use super::transfer::{self, TestFile};
use crate::utils::remote_join;
use std::sync::atomic::AtomicBool;

pub(crate) const LEVELS: [usize; 6] = [1, 2, 4, 8, 16, 32];
pub(crate) const FILE_BYTES: u64 = MIB;
const FILES_PER_SLOT: usize = 2;
const MIN_FILES: usize = 4;
/// A level must be this much faster than the best so far to count as a gain.
const GAIN: f64 = 1.10;
/// Levels without gain after which climbing stops.
const FLAT_LEVELS: usize = 2;
/// The recommendation reaches at least this share of the best rate.
const ENOUGH: f64 = 0.90;

fn files_at(level: usize) -> usize {
    (level * FILES_PER_SLOT).max(MIN_FILES)
}

/// Bytes the tuning of one remote uploads at most (progress bar share).
pub(crate) fn expected_bytes(plan: &TestPlan) -> u64 {
    if !plan.tune_uploads {
        return 0;
    }
    LEVELS
        .iter()
        .map(|&level| files_at(level) as u64 * FILE_BYTES)
        .sum()
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
    pub tuning: UploadTuning,
    /// Bytes reported to the progress bar.
    pub reported: u64,
    /// Level folders that may be left behind.
    pub leftover: Vec<String>,
}

/// Runs the levels on one remote; never fails the remote.
pub(crate) fn run(
    run: &Run<'_>,
    at: &Position<'_>,
    test_dir: &str,
    account: Option<String>,
) -> TuneOutcome {
    let engine = run.engine;
    let current = account
        .as_deref()
        .and_then(|account| crate::storage::account::runtime::settings().max_uploads(account));
    let mut steps = Vec::new();
    let mut reported = 0;
    let mut leftover = Vec::new();
    for level in LEVELS {
        if engine.cancelled() || !climb_further(&steps) {
            break;
        }
        at.status(&format!("tuning uploads: {level} at once"));
        let dir = remote_join(test_dir, &format!("{}-tune-{level:02}", run.run_id));
        let files: Vec<TestFile> = (0..files_at(level))
            .map(|i| TestFile {
                address: remote_join(&dir, &format!("{i:04}.bin")),
                size: FILE_BYTES,
            })
            .collect();
        let attempted: Vec<AtomicBool> = files.iter().map(|_| AtomicBool::new(false)).collect();
        let (phase, progress) = {
            let _uncapped = crate::storage::rclone::uncap_for_speed_test();
            // Distinct contents per level: no provider-side dedupe.
            transfer::upload(
                engine,
                at,
                &files,
                level,
                &level_seed(&run.seed, level),
                &attempted,
            )
        };
        reported += progress.reported();
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        let error = if engine.cancelled() {
            Some("cancelled".to_owned())
        } else {
            phase.error().map(str::to_owned)
        };
        steps.push(TuningStep {
            parallel: level,
            files: files.len(),
            bytes_per_s: (error.is_none() && phase.complete())
                .then(|| bytes as f64 / phase.wall.as_secs_f64().max(1e-3)),
            error,
        });
        at.status("cleaning up");
        if !cleanup::remove_run(engine, &files, &attempted, level, &dir, test_dir) {
            leftover.push(dir);
        }
    }
    let recommended = recommend(&steps);
    TuneOutcome {
        tuning: UploadTuning {
            account,
            file_bytes: FILE_BYTES,
            steps,
            recommended,
            current,
        },
        reported,
        leftover,
    }
}

fn level_seed(seed: &[u8; 32], level: usize) -> [u8; 32] {
    *blake3::keyed_hash(seed, format!("tune-{level}").as_bytes()).as_bytes()
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
        let mut plan = TestPlan {
            bytes_per_remote: MIB,
            file_sizes: vec![MIB],
            parallel: 1,
            tune_uploads: false,
        };
        assert_eq!(expected_bytes(&plan), 0);
        plan.tune_uploads = true;
        // 4 + 4 + 8 + 16 + 32 + 64 files of 1 MiB.
        assert_eq!(expected_bytes(&plan), 128 * MIB);
    }
}
