//! Orchestrator (work package C): create a migration (plan + publish), run it
//! with resume, abandon it.
use super::model::Plan;
use super::plan::PlanOptions;
use crate::prelude::*;

/// Plans `pool` and publishes the plan to the cloud journal.
pub(crate) fn create(rclone: &str, pool: &str, options: &PlanOptions) -> Result<Plan> {
    let _ = (rclone, pool, options);
    bail!("migration not implemented yet")
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunOptions {
    pub stop_file: Option<PathBuf>,
}

/// Runs (or resumes) `migration_id` of `pool` until every movable entry is
/// switched, the stop file appears, or an error stops it. Never deletes.
pub(crate) fn run(
    rclone: &str,
    pool: &str,
    migration_id: &str,
    options: &RunOptions,
) -> Result<()> {
    let _ = (rclone, pool, migration_id, options);
    bail!("migration not implemented yet")
}

/// Marks a migration abandoned (other PCs stop offering it).
pub(crate) fn abandon(rclone: &str, pool: &str, migration_id: &str) -> Result<()> {
    let _ = (rclone, pool, migration_id);
    bail!("migration not implemented yet")
}
