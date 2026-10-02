//! Storage speed test: which account of a pool is the bottleneck.
//!
//! `rpool pool speed-test <NAME>` / `rpool provider speed-test --remote R...`
//! write a few random test files under `<remote>/.rpool-speedtest/<run id>/`
//! on every remote, read them back, verify them and delete them, then report
//! per-remote latency and throughput plus an estimate for the pool.
//! `--json` prints one [`model::SpeedTestReport`]; the GUI reads that.
//!
//! Remotes are tested one after another so each number belongs to that
//! account alone. Per remote: a stat of the not-yet-existing run folder
//! (`first_op_ms`, includes the provider's cold start), the median of three
//! more (`latency_ms`), a best-effort free-space check (`rclone about`), the
//! parallel upload, the parallel read-back with BLAKE3 comparison and the
//! cleanup (`cleanup`). Progress: see `progress`; stop: see `cancel`.
//! `--tune-uploads` adds a per-remote search for the best number of
//! simultaneous uploads (`tune`).
mod cancel;
mod cleanup;
mod command;
mod engine;
mod estimate;
mod format;
pub(crate) mod model;
mod options;
mod progress;
pub(crate) mod remaining;
mod remote;
mod run;
mod stream;
mod transfer;
mod tune;

#[cfg(all(test, unix))]
mod integration_tests;

pub(crate) use command::{run_pool, run_remotes};
pub(crate) use remote::backend_type;
pub(crate) use tune::TUNE_SHARD_BYTES;
