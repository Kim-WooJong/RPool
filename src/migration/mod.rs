//! Pool change migration: when a pool's accounts or coding change, move every
//! affected archive onto the new policy. Plan first (what moves, bytes, ETA,
//! what is already unrecoverable), then run with a journal in the cloud so any
//! PC can resume. Archives switch one by one; the drive's files are migrated
//! the same way and then adopted into a new drive generation (`drive_*`, see
//! docs/POOL_MIGRATION_DESIGN.md).
//!
//! CONTRACT between the work packages: `model` types and the signatures in
//! `plan`, `journal`, `relocate`, `speed`, `execute` and `status` are shared; keep them stable.
mod classify;
pub(crate) mod drive_adopt;
mod drive_adopt_live;
pub(crate) mod drive_generations;
pub(crate) mod drive_journal;
pub(crate) mod drive_model;
mod drive_plan;
pub(crate) mod drive_run;
pub(crate) mod drive_source;
pub(crate) mod drive_status;
#[cfg(test)]
pub(crate) mod drive_test_support;
pub(crate) mod enumerate;
mod estimate;
pub(crate) mod execute;
pub(crate) mod journal;
pub(crate) mod model;
pub(crate) mod plan;
mod probe;
pub(crate) mod rebalance;
pub(crate) mod relocate;
pub(crate) mod retire;
pub(crate) mod speed;
pub(crate) mod status;
#[cfg(test)]
pub(crate) mod test_support;
