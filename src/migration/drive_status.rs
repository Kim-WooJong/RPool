//! Drive status of a migration: the frozen drive plan folded with the
//! journal records, the freeze and the adoption marker. Lost drive files use
//! the archive `LostFile` rows (`original_name` is the drive path,
//! `archive_id` the payload archive when planned).
use super::drive_journal::{load_adoption, load_freeze, load_plan};
use super::drive_model::{DriveAdoption, DriveFreeze, DrivePlan, DriveStatus};
use super::execute::{progress, Progress};
use super::journal::Journal;
use super::model::{Action, LostFile, Record};
use crate::prelude::*;

/// Drive status of `migration_id`; `Ok(None)` when it has no drive part.
pub(crate) fn status(rclone: &str, pool: &str, migration_id: &str) -> Result<Option<DriveStatus>> {
    let journal = Journal::open(rclone, pool, migration_id)?;
    let Some(drive) = load_plan(&journal)? else {
        return Ok(None);
    };
    let records = journal.records()?;
    Ok(Some(summarize(
        &drive,
        &records,
        load_freeze(&journal)?.as_ref(),
        load_adoption(&journal)?,
    )))
}

/// Folds the drive plan with journal records, freeze and adoption into a
/// [`DriveStatus`]: counts of switched/verified/failed entries, lost files
/// (from the plan and from the run) and whether adoption is ready.
pub(crate) fn summarize(
    drive: &DrivePlan,
    records: &[Record],
    freeze: Option<&DriveFreeze>,
    adopted: Option<DriveAdoption>,
) -> DriveStatus {
    let state = progress(records);
    let mut lost: BTreeMap<String, LostFile> = BTreeMap::new();
    let row = |entry: &super::drive_model::DriveEntry, groups, detected: &str| LostFile {
        archive_id: entry.source_archive_id.clone(),
        original_name: entry.path.clone(),
        size: entry.size,
        groups,
        detected: detected.into(),
    };
    let (mut to_move, mut switched, mut verified, mut failed_unknown, mut left) = (0, 0, 0, 0, 0);
    for entry in &drive.entries {
        if entry.action == Action::Lost {
            lost.insert(entry.key.clone(), row(entry, entry.losses.clone(), "plan"));
            continue;
        }
        if !matches!(entry.action, Action::Relocate | Action::Reencode) {
            continue;
        }
        to_move += 1;
        match state.get(&entry.key) {
            Some(Progress::Switched(_)) => switched += 1,
            Some(Progress::Verified(_)) => {
                verified += 1;
                left += 1;
            }
            Some(Progress::Lost(r)) => {
                lost.insert(entry.key.clone(), row(entry, r.losses.clone(), "run"));
            }
            Some(Progress::Unknown(_)) => {
                failed_unknown += 1;
                left += 1;
            }
            Some(Progress::Claimed(_)) | None => left += 1,
        }
    }
    DriveStatus {
        migration_id: drive.migration_id.clone(),
        source: drive.source.clone(),
        epoch: drive.epoch.clone(),
        counts: drive.counts.clone(),
        to_move,
        switched,
        verified,
        failed_unknown,
        lost: lost.into_values().collect(),
        frozen: freeze.is_some(),
        ready: left == 0,
        adopted,
        bootstrap_ok: drive.bootstrap_ok,
    }
}
