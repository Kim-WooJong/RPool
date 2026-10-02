//! A whole speed test: every remote one after another, then bottlenecks and
//! the pool estimate.
use super::engine::Engine;
use super::estimate::{self, Shares};
use super::model::{SpeedTestReport, REPORT_VERSION};
use super::options::TestPlan;
use super::progress::Position;
use super::remote::{self, Run};
use super::stream::random_seed;
use crate::prelude::*;
use std::time::Duration;

/// The pool under test.
pub(crate) struct PoolTarget<'a> {
    pub name: &'a str,
    pub definition: &'a PoolDefinition,
}

fn run_id() -> Result<String> {
    let mut id = [0u8; 8];
    getrandom::fill(&mut id).map_err(|e| anyhow!("random run id: {e}"))?;
    Ok(id.iter().map(|b| format!("{b:02x}")).collect())
}

/// Tests `remotes` in order. Per-remote failures are in the report; only
/// setup errors (no randomness) fail the call.
pub(crate) fn execute(
    engine: &Engine,
    rclone: &str,
    remotes: &[String],
    plan: &TestPlan,
    pool: Option<PoolTarget<'_>>,
) -> Result<SpeedTestReport> {
    let per_remote = 2 * plan.bytes_per_remote + super::tune::expected_bytes(plan);
    crate::progress::start((remotes.len() as u64).saturating_mul(per_remote));
    // Read daemon start-up is not any remote's cold start.
    engine.context.warm_read_daemon();
    let config = engine
        .context
        .config_dump(&engine.step(Duration::from_secs(30)))
        .ok();
    let run = Run {
        engine,
        plan,
        seed: random_seed()?,
        run_id: run_id()?,
        config: config.as_ref(),
    };
    let mut speeds = Vec::with_capacity(remotes.len());
    let mut leftovers = Vec::new();
    for (index, remote) in remotes.iter().enumerate() {
        let at = Position {
            remote,
            index: index + 1,
            count: remotes.len(),
        };
        let outcome = remote::test(&run, &at);
        speeds.push(outcome.speed);
        leftovers.extend(outcome.leftover);
    }
    crate::progress::finish();

    let up: Vec<Option<f64>> = speeds
        .iter()
        .map(|s| s.ok.then_some(s.upload_bytes_per_s).flatten())
        .collect();
    let down: Vec<Option<f64>> = speeds
        .iter()
        .map(|s| s.ok.then_some(s.download_bytes_per_s).flatten())
        .collect();
    let shares: Option<Shares> = pool.as_ref().and_then(|pool| {
        if up.iter().all(Option::is_none) {
            return None;
        }
        let roots = crate::remote_root::apply_remote_roots(remotes.to_vec()).ok()?;
        estimate::placement_shares(rclone, &roots, pool.definition).ok()
    });
    let (bottleneck_up, bottleneck_down) = match &shares {
        Some(shares) => (
            estimate::weighted_bottleneck(&shares.all, &up),
            estimate::weighted_bottleneck(&shares.data, &down),
        ),
        None => (
            estimate::raw_bottleneck(&up),
            estimate::raw_bottleneck(&down),
        ),
    };
    let estimate = match (&pool, &shares) {
        (Some(pool), Some(shares)) => estimate::pool_estimate(
            pool.definition.data_shards,
            pool.definition.parity_shards,
            shares,
            &up,
            &down,
        ),
        _ => None,
    };
    let name = |i: Option<usize>| i.map(|i| speeds[i].remote.clone());
    Ok(SpeedTestReport {
        version: REPORT_VERSION,
        pool: pool.map(|p| p.name.to_owned()),
        bytes_per_remote: plan.bytes_per_remote,
        files_per_remote: plan.files(),
        parallel: plan.parallel,
        estimate,
        bottleneck_upload: name(bottleneck_up),
        bottleneck_download: name(bottleneck_down),
        remotes: speeds,
        leftovers,
    })
}
