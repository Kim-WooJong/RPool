//! Access to the pool's drive in the cloud for planning and adoption: which
//! generations exist (adoption-aware) and the files of one generation. No
//! workspace is involved; everything is read from the pool-sync / snapshot
//! records on the pool's remotes. Faked in tests.
use super::drive_generations::{effective, generation_ref};
use super::drive_model::{GenerationRef, SourceView};
use crate::prelude::*;

pub(crate) trait DriveSource: Sync {
    /// Generations holding drive records, best first. Empty: the pool has
    /// no drive.
    fn generations(&self) -> Result<Vec<GenerationRef>>;
    /// The files of `generation` (a fresh mount's view).
    fn view(&self, generation: &GenerationRef) -> Result<SourceView>;
}

/// Production source over rclone, for the saved (new) pool policy.
pub(crate) struct CloudDrive {
    rclone: String,
    pool: String,
    policy: PoolDefinition,
}

impl CloudDrive {
    pub(crate) fn new(rclone: &str, pool: &str, policy: &PoolDefinition) -> Self {
        Self {
            rclone: rclone.into(),
            pool: pool.into(),
            policy: policy.clone(),
        }
    }
}

impl DriveSource for CloudDrive {
    fn generations(&self) -> Result<Vec<GenerationRef>> {
        let listed = crate::pool::browse_generations::discover(
            &self.rclone,
            &self.pool,
            &self.policy.remotes,
        )?;
        let known = super::drive_journal::known(&self.rclone, &self.pool)?;
        Ok(effective(listed, &known)
            .iter()
            .map(generation_ref)
            .collect())
    }
    fn view(&self, generation: &GenerationRef) -> Result<SourceView> {
        crate::mount::drive_generation_read::read(
            &self.rclone,
            &self.pool,
            &self.policy,
            generation,
        )
    }
}
