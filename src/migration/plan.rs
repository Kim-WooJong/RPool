//! Planner (work package A): classify every archive of a pool against its
//! saved policy, estimate bytes and time, and list unrecoverable archives.
use super::model::Plan;
use crate::prelude::*;

#[derive(Debug, Clone, Default)]
pub(crate) struct PlanOptions {
    /// Hash every shard of affected archives instead of listing sizes.
    pub probe_full: bool,
    pub download_mib_s: Option<f64>,
    pub upload_mib_s: Option<f64>,
    pub workers: usize,
}

/// Builds a plan for `pool` (the saved pool is the target policy). Read-only:
/// nothing is written anywhere. Blocking; may take a while.
pub(crate) fn plan(rclone: &str, pool: &str, options: &PlanOptions) -> Result<Plan> {
    let _ = (rclone, pool, options);
    bail!("migration planner not implemented yet")
}
