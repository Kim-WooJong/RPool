//! Status (work package C): fold the journal of each migration into a
//! summary, including the list of unrecoverable files.
use super::model::MigrationStatus;
use crate::prelude::*;

/// Migrations of `pool` found in the cloud (or only `migration_id`), newest first.
pub(crate) fn status(
    rclone: &str,
    pool: &str,
    migration_id: Option<&str>,
) -> Result<Vec<MigrationStatus>> {
    let _ = (rclone, pool, migration_id);
    bail!("migration status not implemented yet")
}
