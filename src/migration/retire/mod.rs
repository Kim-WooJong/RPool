//! Phase 4 of pool change migration: clean up what a completed migration
//! left behind (`rpool pool migrate retire` / `restore`, GUI "Clean up").
//!
//! - `plan`: which originals and orphan copies may go, and why the rest stays.
//! - `refs` / `live_refs`: references (manifests, drive metadata, other
//!   migrations) checked fresh right before quarantine and before deletion.
//! - `fossil`: two-step deletion. Quarantine is a journal record only; the
//!   objects stay readable until a later run after the grace period.
//! - `guard`: mass-delete guard. `execute`: the steps. `restore`: undo a
//!   quarantine. `io` / `live` / `observe`: the side-effect boundary.
pub(crate) mod execute;
mod fossil;
pub(crate) mod guard;
pub(crate) mod io;
pub(crate) mod live;
pub(crate) mod live_refs;
pub(crate) mod model;
pub(crate) mod observe;
pub(crate) mod plan;
pub(crate) mod refs;
pub(crate) mod restore;
#[cfg(test)]
mod test_fixture;

/// Dry run or confirmed cleanup of `migration_id` (see [`execute::retire`]).
pub(crate) fn retire(
    rclone: &str,
    pool: &str,
    migration_id: &str,
    options: &model::RetireOptions,
) -> anyhow::Result<model::RetireReport> {
    execute::retire(&live::LiveIo::open(rclone, pool, migration_id)?, options)
}

/// Takes items out of quarantine (see [`restore::restore`]).
pub(crate) fn restore(
    rclone: &str,
    pool: &str,
    migration_id: &str,
    items: &[String],
    all: bool,
) -> anyhow::Result<Vec<String>> {
    restore::restore(&live::LiveIo::open(rclone, pool, migration_id)?, items, all)
}
