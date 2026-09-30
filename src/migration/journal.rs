//! Cloud journal (work package B): the frozen plan and append-only progress
//! records of one migration, replicated to the pool's remotes under
//! `.rpool-sync/migrations-v1/<scope>/<migration_id>/`, encrypted like pool
//! sync metadata, with a local cache for offline status.
use super::model::{Plan, Record};
use crate::prelude::*;

pub(crate) struct Journal {
    _private: (),
}

impl Journal {
    /// Opens the journal of `migration_id` for `pool` (uses the pool's saved
    /// remotes; nothing is written until a publish/append).
    pub(crate) fn open(rclone: &str, pool: &str, migration_id: &str) -> Result<Self> {
        let _ = (rclone, pool, migration_id);
        bail!("migration journal not implemented yet")
    }
    /// Writes the frozen plan (idempotent: the same plan may be published again).
    pub(crate) fn publish_plan(&self, plan: &Plan) -> Result<()> {
        let _ = plan;
        bail!("migration journal not implemented yet")
    }
    pub(crate) fn load_plan(&self) -> Result<Option<Plan>> {
        bail!("migration journal not implemented yet")
    }
    /// Appends one immutable record to every reachable replica.
    pub(crate) fn append(&self, record: &Record) -> Result<()> {
        let _ = record;
        bail!("migration journal not implemented yet")
    }
    /// All records from all reachable replicas (duplicates allowed).
    pub(crate) fn records(&self) -> Result<Vec<Record>> {
        bail!("migration journal not implemented yet")
    }
}

/// Migration ids recorded in the cloud for `pool`, newest first.
pub(crate) fn discover(rclone: &str, pool: &str) -> Result<Vec<String>> {
    let _ = (rclone, pool);
    bail!("migration journal not implemented yet")
}
