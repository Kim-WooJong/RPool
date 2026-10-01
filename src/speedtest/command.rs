//! CLI entry points: `rpool pool speed-test` and `rpool provider speed-test`.
use super::engine::Engine;
use super::options::TestPlan;
use super::run::{execute, PoolTarget};
use crate::cli::pool::SpeedTestSizeArgs;
use crate::prelude::*;
use crate::storage::rclone::RcloneContext;

/// Prints the report; a stop is an error after the report (cleanup ran).
fn finish(engine: &Engine, report: &super::model::SpeedTestReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        print!("{}", super::format::render(report));
    }
    if engine.cancelled() {
        bail!("speed test cancelled");
    }
    Ok(())
}

pub(crate) fn run_pool(rclone: &str, name: &str, size: SpeedTestSizeArgs) -> Result<()> {
    let store = crate::pool::load_pool_store()?;
    let pool = store
        .pools
        .get(name)
        .ok_or_else(|| anyhow!("pool not found: {name}"))?;
    if pool.remotes.is_empty() {
        bail!("pool {name} has no remotes");
    }
    let plan = TestPlan::new(&size, Some(pool.workers))?;
    let engine = Engine::new(
        RcloneContext::inherited(rclone),
        pool.native_crypt,
        super::cancel::install(),
    );
    let report = execute(
        &engine,
        rclone,
        &pool.remotes,
        &plan,
        Some(PoolTarget {
            name,
            definition: pool,
        }),
    )?;
    finish(&engine, &report, size.json)
}

pub(crate) fn run_remotes(
    rclone: &str,
    remotes: Vec<String>,
    size: SpeedTestSizeArgs,
) -> Result<()> {
    if remotes.is_empty() {
        bail!("give at least one --remote");
    }
    for remote in &remotes {
        crate::storage::rclone::remote_name(remote)
            .map_err(|e| anyhow!("--remote {remote}: {e}"))?;
    }
    let plan = TestPlan::new(&size, None)?;
    let engine = Engine::new(
        RcloneContext::inherited(rclone),
        false,
        super::cancel::install(),
    );
    let report = execute(&engine, rclone, &remotes, &plan, None)?;
    finish(&engine, &report, size.json)
}
