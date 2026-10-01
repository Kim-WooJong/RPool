//! The drive documents of a migration's cloud journal (`drive-plan.json`,
//! `drive-freeze.json`, `drive-adoption.json`; see `drive_model`), and the
//! scan of every migration of a pool that mounts, browse and planning use to
//! find the current drive generation ([`known`]). Documents are write-once;
//! copies written by different PCs may differ only in who and when, and the
//! earliest copy wins.
use super::drive_generations::Known;
use super::drive_model::{
    DriveAdoption, DriveFreeze, DrivePlan, DRIVE_ADOPTION, DRIVE_FREEZE, DRIVE_PLAN,
};
use super::journal::{Journal, JournalStore};
use crate::prelude::*;
use std::sync::Arc;

/// Publishes the frozen drive plan. A different plan already stored is refused.
pub(crate) fn publish_plan(journal: &Journal, plan: &DrivePlan) -> Result<()> {
    check_owner(journal, &plan.migration_id)?;
    let bytes = serde_json::to_vec(plan)?;
    if let Some(existing) = journal.create_document(DRIVE_PLAN, &bytes)? {
        let same = serde_json::from_slice::<Value>(&existing).ok()
            == serde_json::from_slice::<Value>(&bytes).ok();
        if !same {
            bail!(
                "a different drive plan already exists for migration {}; refusing to overwrite",
                plan.migration_id
            );
        }
    }
    Ok(())
}

fn check_owner(journal: &Journal, migration_id: &str) -> Result<()> {
    if journal.migration_id() != migration_id {
        bail!("drive document belongs to another migration");
    }
    Ok(())
}

/// The drive plan, if this migration has a drive part.
pub(crate) fn load_plan(journal: &Journal) -> Result<Option<DrivePlan>> {
    let mut found: Option<(Value, DrivePlan)> = None;
    for bytes in journal.read_documents(DRIVE_PLAN, true)? {
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Ok(plan) = serde_json::from_value::<DrivePlan>(value.clone()) else {
            continue;
        };
        if plan.migration_id != journal.migration_id() {
            continue;
        }
        match &found {
            Some((first, _)) if *first != value => {
                bail!("drive plan replicas disagree; refusing to continue")
            }
            Some(_) => {}
            None => found = Some((value, plan)),
        }
    }
    Ok(found.map(|(_, plan)| plan))
}

/// Earliest valid copy of a marker document.
fn earliest<T: serde::de::DeserializeOwned>(
    journal: &Journal,
    rel: &str,
    key: impl Fn(&T) -> (u64, String),
    owner: impl Fn(&T) -> &str,
) -> Result<Option<T>> {
    let mut copies: Vec<T> = journal
        .read_documents(rel, true)?
        .iter()
        .filter_map(|bytes| serde_json::from_slice::<T>(bytes).ok())
        .filter(|value| owner(value) == journal.migration_id())
        .collect();
    copies.sort_by_key(|value| key(value));
    Ok(copies.into_iter().next())
}

pub(crate) fn load_freeze(journal: &Journal) -> Result<Option<DriveFreeze>> {
    earliest(
        journal,
        DRIVE_FREEZE,
        |f: &DriveFreeze| (f.ts_unix, f.pc_id.clone()),
        |f| &f.migration_id,
    )
}

pub(crate) fn load_adoption(journal: &Journal) -> Result<Option<DriveAdoption>> {
    earliest(
        journal,
        DRIVE_ADOPTION,
        |a: &DriveAdoption| (a.ts_unix, a.pc_id.clone()),
        |a| &a.migration_id,
    )
}

/// Writes the freeze (or keeps the one already there) and returns the
/// winning freeze.
pub(crate) fn freeze(journal: &Journal, freeze: &DriveFreeze) -> Result<DriveFreeze> {
    check_owner(journal, &freeze.migration_id)?;
    journal.create_document(DRIVE_FREEZE, &serde_json::to_vec(freeze)?)?;
    let winner = load_freeze(journal)?.unwrap_or_else(|| freeze.clone());
    if winner.source != freeze.source || winner.epoch != freeze.epoch {
        bail!("the recorded drive freeze names another generation; refusing to continue");
    }
    Ok(winner)
}

/// Writes the adoption marker (the commit point) or keeps an equivalent one
/// already there; returns the winning marker.
pub(crate) fn adopt(journal: &Journal, adoption: &DriveAdoption) -> Result<DriveAdoption> {
    check_owner(journal, &adoption.migration_id)?;
    journal.create_document(DRIVE_ADOPTION, &serde_json::to_vec(adoption)?)?;
    let winner = load_adoption(journal)?.unwrap_or_else(|| adoption.clone());
    if !winner.same_meaning(adoption) {
        bail!("another adoption of this migration is recorded with a different generation");
    }
    Ok(winner)
}

/// Every drive migration of `pool` recorded in the cloud (and the local
/// cache). Errors only when no journal store could be listed.
pub(crate) fn known(rclone: &str, pool: &str) -> Result<Known> {
    let (cloud, cache) = super::journal::pool_stores(rclone, pool)?;
    known_in(pool, cloud, cache)
}

pub(crate) fn known_in(
    pool: &str,
    cloud: Vec<Arc<dyn JournalStore>>,
    cache: Option<Arc<dyn JournalStore>>,
) -> Result<Known> {
    let ids = super::journal::list_ids(&cloud, cache.as_ref())?;
    let found: Vec<Result<(Option<DriveAdoption>, Option<DriveFreeze>)>> = ids
        .par_iter()
        .map(|id| {
            let journal = Journal::with_stores(pool, id, cloud.clone(), cache.clone())?;
            // Archive-only migrations have no drive plan: one read, then skip.
            if journal.read_documents(DRIVE_PLAN, true)?.is_empty() {
                return Ok((None, None));
            }
            if let Some(adoption) = load_adoption(&journal)? {
                return Ok((Some(adoption), None));
            }
            let Some(freeze) = load_freeze(&journal)? else {
                return Ok((None, None));
            };
            let abandoned = super::execute::is_abandoned(&journal.records()?);
            Ok((None, (!abandoned).then_some(freeze)))
        })
        .collect();
    let mut known = Known {
        migration_ids: ids,
        ..Default::default()
    };
    for result in found {
        let (adoption, freeze) = result?;
        known.adoptions.extend(adoption);
        known.freezes.extend(freeze);
    }
    Ok(known)
}
