//! Reads a drive's history: from an open workspace (after a metadata pull),
//! or read-only straight from the cloud like `rpool pool browse` (v6 events
//! including checkpointed ones).
use super::graph::History;
use crate::mount::history_bridge::{self as bridge, VirtualDrive};
use crate::pool::browse_generations::Generation;
use crate::prelude::*;

pub(crate) struct Loaded {
    pub history: History,
    /// Degraded information worth telling the user (stderr).
    pub notes: Vec<String>,
}

/// History of an open drive. A failed refresh falls back to this PC's
/// last known state (noted), so listing works offline.
pub(crate) fn from_drive(drive: &VirtualDrive, rclone: &str, now: u64) -> Result<Loaded> {
    let mut notes = Vec::new();
    if let Err(error) = drive.pull() {
        notes.push(format!(
            "cloud refresh failed ({error:#}); showing this PC's last known drive state"
        ));
    }
    let roots = bridge::roots(drive).to_vec();
    let native_crypt = drive.policy.native_crypt;
    let (times, time_notes) = super::times::list(rclone, &roots);
    notes.extend(time_notes);
    let (purged, cleaned) = marks(rclone, &roots, native_crypt, &mut notes);
    let (events, unpublished) = bridge::v6_events(drive);
    let mut history = super::source_v6::build(events, &times, &unpublished, now, purged)?;
    forget_cleaned(&mut history, &cleaned);
    Ok(Loaded { history, notes })
}

/// Purged trash ids and archives the cleanup deleted (or is deleting).
fn marks(
    rclone: &str,
    roots: &[String],
    native_crypt: bool,
    notes: &mut Vec<String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let result = super::marks::Remote::open(rclone, roots, native_crypt).and_then(|stores| {
        let stores: Vec<&dyn super::marks::MarkStore> = stores.iter().map(|s| s as _).collect();
        Ok((
            super::marks::purged(&stores)?,
            super::cleanup::records::unrestorable(&super::cleanup::records::read(&stores)?),
        ))
    });
    result.unwrap_or_else(|error| {
        notes.push(format!(
            "purge marks unavailable ({error:#}); purged entries may show and restoring cleaned-up data fails"
        ));
        (BTreeSet::new(), BTreeSet::new())
    })
}

/// Revisions whose data the cleanup removed are no longer restorable.
pub(crate) fn forget_cleaned(history: &mut History, cleaned: &BTreeSet<String>) {
    if cleaned.is_empty() {
        return;
    }
    history.payloads.retain(|_, payload| {
        super::cleanup::select::folders(&payload.manifest).is_disjoint(cleaned)
    });
}

/// The drive generation a workspace-less command uses: the newest one, as
/// `rpool pool browse` shows (`None`: the pool has no pool-sync drive).
pub(crate) fn generation(rclone: &str, pool: &str) -> Result<Option<Generation>> {
    let policy = pool_definition(pool)?;
    let generations = crate::pool::browse_generations::discover(rclone, pool, &policy.remotes)?;
    let known = crate::migration::drive_journal::known(rclone, pool)
        .context("cannot check this pool's migrations")?;
    Ok(
        crate::migration::drive_generations::effective(generations, &known)
            .into_iter()
            .next(),
    )
}

pub(crate) fn pool_definition(pool: &str) -> Result<PoolDefinition> {
    let policy = crate::pool::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .with_context(|| format!("pool not found: {pool}"))?;
    crate::pool::validate_pool(&policy)?;
    Ok(policy)
}

/// Metadata roots of `generation` on every replica.
pub(crate) fn generation_roots(pool: &str, generation: &Generation) -> Result<Vec<String>> {
    let policy = pool_definition(pool)?;
    Ok(crate::mount::pool_sync::roots(pool, &policy.remotes)?
        .into_iter()
        .map(|root| match &generation.epoch {
            Some(epoch) => crate::utils::remote_join(&root, &format!("epochs/{epoch}")),
            None => root,
        })
        .collect())
}

/// Read-only history from the cloud (no workspace; nothing is published).
pub(crate) fn from_cloud(rclone: &str, pool: &str, now: u64) -> Result<Loaded> {
    let generation = generation(rclone, pool)?
        .with_context(|| format!("pool {pool} has no online drive (pool-sync metadata) yet"))?;
    let policy = pool_definition(pool)?;
    let roots = generation_roots(pool, &generation)?;
    let mut notes = Vec::new();
    let (times, time_notes) = super::times::list(rclone, &roots);
    notes.extend(time_notes);
    let (purged, cleaned) = marks(rclone, &roots, policy.native_crypt, &mut notes);
    let events =
        crate::mount::metadata_pool::read_v6(rclone, pool, &policy, generation.epoch.as_deref())?;
    let mut history = super::source_v6::build(events, &times, &BTreeSet::new(), now, purged)?;
    forget_cleaned(&mut history, &cleaned);
    Ok(Loaded { history, notes })
}
