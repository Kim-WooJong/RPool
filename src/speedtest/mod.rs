//! Storage speed test: which account of a pool is the bottleneck.
//!
//! `rpool pool speed-test <NAME>` / `rpool provider speed-test --remote R...`
//! write a few random test files under `<remote>/.rpool-speedtest/<run id>/`
//! on every remote, read them back, verify them and delete them, then report
//! per-remote latency and throughput plus an estimate for the pool.
//! `--json` prints one [`model::SpeedTestReport`]; the GUI reads that.
pub(crate) mod model;
