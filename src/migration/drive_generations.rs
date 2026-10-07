//! Which drive generation is current, given the migrations recorded in the
//! cloud. Pure decisions over [`Known`] (read by `drive_journal::known`):
//!
//! - a generation that is the source of an adoption is superseded, even when
//!   a PC still on it wrote newer records (those late writes were not
//!   migrated; the old generation itself is kept, never deleted);
//! - a migration epoch without its adoption marker is a partial publication
//!   and is never read;
//! - a fresh workspace opens the newest adopted generation that was not
//!   adopted further;
//! - a workspace on a superseded generation is refused (adopt it), one on a
//!   frozen generation may mount but must not publish.
use super::drive_model::{epoch_for, DriveAdoption, DriveFreeze, GenerationRef};
use crate::pool::browse_generations::Generation;
use crate::prelude::*;

/// Drive migrations recorded for a pool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Known {
    /// Adoption markers of every adopted migration.
    pub adoptions: Vec<DriveAdoption>,
    /// Freezes of migrations neither adopted nor abandoned.
    pub freezes: Vec<DriveFreeze>,
    /// Every migration id (their epochs are migration epochs).
    pub migration_ids: BTreeSet<String>,
}

impl Known {
    /// The latest adoption whose source is `generation`, if it was moved on.
    fn superseded(&self, generation: &GenerationRef) -> Option<&DriveAdoption> {
        self.adoptions
            .iter()
            .filter(|a| a.source == *generation)
            .max_by(|a, b| (a.ts_unix, &a.epoch).cmp(&(b.ts_unix, &b.epoch)))
    }
    /// Some adoption created the generation `epoch`.
    fn adopted(&self, epoch: &str) -> bool {
        self.adoptions.iter().any(|a| a.epoch == epoch)
    }
    /// Epochs of all recorded migrations (adopted or not).
    fn migration_epochs(&self) -> BTreeSet<String> {
        self.migration_ids.iter().map(|id| epoch_for(id)).collect()
    }
}

/// The reference used in migration documents for a browse generation.
pub(crate) fn generation_ref(generation: &Generation) -> GenerationRef {
    GenerationRef {
        epoch: generation.epoch.clone(),
    }
}

/// Generations a reader may use, best first: adopted generations first, then
/// the others newest first (the order `discover` returns). Superseded and
/// partially published migration generations are dropped.
pub(crate) fn effective(generations: Vec<Generation>, known: &Known) -> Vec<Generation> {
    let partial = known.migration_epochs();
    let mut kept: Vec<(bool, Generation)> = generations
        .into_iter()
        .filter(|g| known.superseded(&generation_ref(g)).is_none())
        .filter_map(|g| {
            let adopted = g.epoch.as_deref().is_some_and(|e| known.adopted(e));
            let incomplete = g.epoch.as_ref().is_some_and(|e| partial.contains(e)) && !adopted;
            (!incomplete).then_some((adopted, g))
        })
        .collect();
    // Stable: keeps the newest-first order within each group.
    kept.sort_by_key(|(adopted, _)| !*adopted);
    kept.into_iter().map(|(_, g)| g).collect()
}

/// The generation a NEW workspace opens, if the drive was adopted: the
/// newest adoption not adopted further.
pub(crate) fn fresh_workspace(known: &Known) -> Option<&DriveAdoption> {
    known
        .adoptions
        .iter()
        .filter(|a| known.superseded(&a.generation()).is_none())
        .max_by(|a, b| (a.ts_unix, &a.epoch).cmp(&(b.ts_unix, &b.epoch)))
}

/// What a workspace on `generation` may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fence {
    /// No migration involves this generation.
    Proceed,
    /// A migration is moving this drive: mount, but do not publish.
    Frozen(DriveFreeze),
    /// The drive moved to a new generation: refuse; adopt instead.
    Superseded(DriveAdoption),
}

/// Decides what a workspace on `generation` may do: superseded wins over
/// frozen. Used by mounts and sync before publishing.
pub(crate) fn fence(generation: &GenerationRef, known: &Known) -> Fence {
    if let Some(adoption) = known.superseded(generation) {
        return Fence::Superseded(adoption.clone());
    }
    match known.freezes.iter().find(|f| f.source == *generation) {
        Some(freeze) => Fence::Frozen(freeze.clone()),
        None => Fence::Proceed,
    }
}

impl Fence {
    /// The refusal a mount or sync shows (None: proceed).
    pub(crate) fn message(&self, pool: &str) -> Option<String> {
        match self {
            Fence::Proceed => None,
            Fence::Frozen(f) => Some(format!(
                "pool migration {} is moving this drive ({}) to a new layout: changes on this PC are kept locally and are not published until you adopt it (`rpool pool migrate adopt {pool} --id {} --workspace <this workspace>` after the migration finishes)",
                f.migration_id,
                f.source.label(),
                f.migration_id
            )),
            Fence::Superseded(a) => Some(format!(
                "this workspace is on the previous drive generation ({}): pool migration {} moved the drive to a new layout. Nothing was deleted. Mounting this workspace switches it automatically; or switch it with `rpool pool migrate adopt {pool} --id {} --workspace <this workspace>` (both keep this workspace as a backup and export local-only writes)",
                a.source.label(),
                a.migration_id,
                a.migration_id
            )),
        }
    }
}

#[cfg(test)]
#[path = "drive_generations_tests.rs"]
mod tests;
