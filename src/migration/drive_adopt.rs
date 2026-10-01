//! Adoption (phase 3): make the migrated drive the pool's drive. Works from
//! the cloud alone (journal + source generation records), so any PC can do
//! it, including one that never had the drive's workspace.
//!
//! 1. The source generation is frozen (`drive-freeze.json`, written by the
//!    run's drive part or here) and has been for [`SETTLE_SECONDS`], longer
//!    than a mounted PC goes without checking (`mount::adoption_fence`).
//! 2. The source's visible files are read again. A file whose revision has
//!    a `Switched` record uses that new archive; a v6 file that needs no
//!    move keeps its archive; anything else (changed after planning, or not
//!    checked) is caught up through the same run state machine.
//! 3. Every file must be resolved; unrecoverable ones are left out only with
//!    `accept_lost` (they stay readable in the source generation if their
//!    shards ever return; nothing is deleted).
//! 4. The new epoch's records (`drive_generation_write`) are published to
//!    every replica, read back the way a fresh PC reads them and compared,
//!    and only then `drive-adoption.json` (the commit point) is written.
//!
//! Every step is idempotent: records are deterministic and write-once, and a
//! repeated adoption returns the recorded marker.
use super::drive_journal::{
    adopt as record_adoption, freeze, load_adoption, load_freeze, load_plan,
};
use super::drive_model::{
    entry_key, DriveAdoption, DriveFile, DriveFreeze, DrivePlan, GenerationRef, BOOTSTRAP_RECORDS,
};
use super::drive_source::DriveSource;
use super::execute::{is_abandoned, progress, Progress, RunOptions, RunSummary};
use super::journal::Journal;
use super::model::{Action, GroupLoss, Plan};
use crate::mount::drive_generation_write::{publish, records, Sink};
use crate::prelude::*;

/// How long a freeze must be in place before adoption: a mounted PC on the
/// source generation checks the fence at least every
/// `mount::adoption_fence::CHECK_INTERVAL` before it publishes.
pub(crate) const SETTLE_SECONDS: u64 = 5 * 60;

/// Side effects of an adoption, faked in tests.
pub(crate) trait AdoptIo: Sync {
    fn source(&self) -> &dyn DriveSource;
    fn sink(&self, epoch: &str) -> Result<Box<dyn Sink + '_>>;
    fn load_manifest(&self, location: &str) -> Result<Manifest>;
    /// Classifies `files` and runs those needing a new archive through the
    /// run state machine. Returns the run summary and the keys of files that
    /// are kept as they are.
    fn catch_up(&self, files: &[DriveFile]) -> Result<(RunSummary, BTreeSet<String>)>;
    fn now(&self) -> u64 {
        crate::utils::now_unix()
    }
    /// Waits `seconds` (the freeze settling). Err when stopped.
    fn wait(&self, seconds: u64) -> Result<()>;
    fn say(&self, line: &str) {
        println!("{line}");
    }
}

/// A drive file that cannot be adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unresolved {
    pub path: String,
    pub reason: String,
}

/// Adopts the drive of `plan` (see the module docs). `pc` names this PC.
pub(crate) fn adopt_core(
    journal: &Journal,
    plan: &Plan,
    drive: &DrivePlan,
    io: &dyn AdoptIo,
    accept_lost: bool,
    pc: &str,
) -> Result<DriveAdoption> {
    if let Some(adoption) = load_adoption(journal)? {
        io.say(&format!(
            "drive_adopted=already epoch={} files={}",
            &adoption.epoch[..12],
            adoption.files
        ));
        return Ok(adoption);
    }
    if is_abandoned(&journal.records()?) {
        bail!(
            "migration {} was abandoned; create a new plan",
            plan.migration_id
        );
    }
    if !drive.bootstrap_ok {
        bail!("the new drive generation would exceed the fresh-bootstrap budget ({BOOTSTRAP_RECORDS} records / 64 MiB); adoption is refused. Nothing was changed");
    }
    settle(journal, drive, io, pc)?;

    let view = io.source().view(&drive.source)?;
    let unaffected: BTreeSet<String> = drive
        .entries
        .iter()
        .filter(|e| e.action == Action::Unaffected)
        .map(|e| e.key.clone())
        .collect();
    let mut resolution = resolve(journal, &view.files, &unaffected)?;
    if !resolution.pending.is_empty() {
        let files: Vec<DriveFile> = view
            .files
            .iter()
            .filter(|f| {
                resolution
                    .pending
                    .contains(&entry_key(&f.path, &f.revision))
            })
            .cloned()
            .collect();
        io.say(&format!("drive_catch_up files={}", files.len()));
        let (summary, kept) = io.catch_up(&files)?;
        io.say(&format!(
            "drive_catch_up switched_now={} lost={} unknown={} claimed_elsewhere={} stopped={}",
            summary.switched_now,
            summary.lost,
            summary.unknown.len(),
            summary.claimed_elsewhere,
            summary.stopped
        ));
        let unaffected: BTreeSet<String> = unaffected.union(&kept).cloned().collect();
        resolution = resolve(journal, &view.files, &unaffected)?;
    }
    if !resolution.pending.is_empty() {
        let list: Vec<String> = resolution
            .unresolved
            .iter()
            .map(|u| format!("{} ({})", u.path, u.reason))
            .collect();
        bail!(
            "{} drive file(s) have no verified new archive yet; run adopt again (use --take-over if another PC stopped): {}",
            list.len(),
            list.join(", ")
        );
    }
    let dropped: Vec<String> = resolution.lost.keys().cloned().collect();
    if !dropped.is_empty() && !accept_lost {
        bail!(
            "{} drive file(s) are unrecoverable; adopt with --accept-lost to leave them out (they are listed by `pool migrate lost`): {}",
            dropped.len(),
            dropped.join(", ")
        );
    }

    // New manifests, checked against their records.
    let adopted: Vec<DriveFile> = view
        .files
        .par_iter()
        .filter_map(|file| {
            let key = entry_key(&file.path, &file.revision);
            match resolution.resolved.get(&key) {
                None => None,
                Some(None) => Some(Ok(file.clone())),
                Some(Some((id, location))) => {
                    Some(io.load_manifest(location).and_then(|manifest| {
                        crate::manifest::validate_manifest(&manifest)?;
                        if manifest.archive_id != *id || manifest.original_size != file.size {
                            bail!("{}: new archive does not match its record", file.path);
                        }
                        Ok(DriveFile {
                            manifest,
                            ..file.clone()
                        })
                    }))
                }
            }
        })
        .collect::<Result<_>>()?;
    let new_records = records(&adopted, &plan.migration_id)?;
    io.say(&format!(
        "drive_publish epoch={} records={} files={}",
        &drive.epoch[..12],
        new_records.len(),
        adopted.len()
    ));
    publish(io.sink(&drive.epoch)?.as_ref(), &new_records)?;

    // Read the new generation back as a PC without a workspace would.
    let generation = GenerationRef {
        epoch: Some(drive.epoch.clone()),
    };
    let seen = io.source().view(&generation)?;
    let identity = |files: &[DriveFile]| -> Result<BTreeMap<String, (String, u64, String)>> {
        files
            .iter()
            .map(|f| {
                Ok((
                    f.path.clone(),
                    (
                        f.hash.clone(),
                        f.size,
                        crate::manifest::manifest_fingerprint(&f.manifest)?,
                    ),
                ))
            })
            .collect()
    };
    if identity(&seen.files)? != identity(&adopted)? {
        bail!("the new drive generation does not read back as published; not adopted (records are kept and the next adoption re-checks them)");
    }
    let marker = record_adoption(
        journal,
        &DriveAdoption {
            version: 1,
            migration_id: plan.migration_id.clone(),
            source: drive.source.clone(),
            epoch: drive.epoch.clone(),
            files: adopted.len(),
            dropped,
            pc_id: pc.into(),
            ts_unix: io.now(),
        },
    )?;
    io.say(&format!(
        "drive_adopted=true epoch={} files={} dropped={} source={}",
        &marker.epoch[..12],
        marker.files,
        marker.dropped.len(),
        marker.source.label()
    ));
    Ok(marker)
}

/// Freezes the source (if the run did not) and waits until the freeze is
/// [`SETTLE_SECONDS`] old.
fn settle(journal: &Journal, drive: &DrivePlan, io: &dyn AdoptIo, pc: &str) -> Result<()> {
    let frozen = match load_freeze(journal)? {
        Some(frozen) => frozen,
        None => freeze(
            journal,
            &DriveFreeze {
                version: 1,
                migration_id: drive.migration_id.clone(),
                source: drive.source.clone(),
                epoch: drive.epoch.clone(),
                pc_id: pc.into(),
                ts_unix: io.now(),
            },
        )?,
    };
    let age = io.now().saturating_sub(frozen.ts_unix);
    if age < SETTLE_SECONDS {
        let wait = SETTLE_SECONDS - age;
        io.say(&format!(
            "drive_settle_wait_seconds={wait} (PCs on {} are stopping publication)",
            frozen.source.label()
        ));
        io.wait(wait)?;
    }
    Ok(())
}

/// Per visible file: resolved (Some(new archive) or None = keep), lost, or
/// still pending.
struct Resolution {
    resolved: BTreeMap<String, Option<(String, String)>>,
    lost: BTreeMap<String, Vec<GroupLoss>>,
    pending: BTreeSet<String>,
    unresolved: Vec<Unresolved>,
}

fn resolve(
    journal: &Journal,
    files: &[DriveFile],
    unaffected: &BTreeSet<String>,
) -> Result<Resolution> {
    let state = progress(&journal.records()?);
    let mut out = Resolution {
        resolved: BTreeMap::new(),
        lost: BTreeMap::new(),
        pending: BTreeSet::new(),
        unresolved: vec![],
    };
    for file in files {
        let key = entry_key(&file.path, &file.revision);
        let pending = |out: &mut Resolution, reason: &str| {
            out.pending.insert(key.clone());
            out.unresolved.push(Unresolved {
                path: file.path.clone(),
                reason: reason.into(),
            });
        };
        match state.get(&key) {
            Some(Progress::Switched(r)) => match (&r.new_archive_id, &r.new_manifest) {
                (Some(id), Some(location)) => {
                    out.resolved
                        .insert(key.clone(), Some((id.clone(), location.clone())));
                }
                _ => pending(&mut out, "switched record without its new archive"),
            },
            Some(Progress::Lost(r)) => {
                out.lost.insert(file.path.clone(), r.losses.clone());
            }
            _ if unaffected.contains(&key) => {
                out.resolved.insert(key.clone(), None);
            }
            Some(Progress::Verified(_)) => pending(&mut out, "verified, not finished"),
            Some(Progress::Claimed(c)) => pending(&mut out, &format!("claimed by {}", c.pc_id)),
            Some(Progress::Unknown(u)) => {
                pending(&mut out, u.detail.as_deref().unwrap_or("provider error"))
            }
            None => pending(&mut out, "changed since planning or not checked"),
        }
    }
    Ok(out)
}

/// `pool migrate adopt`: adopts the drive of migration `id` of `pool`.
pub(crate) fn adopt(
    rclone: &str,
    pool: &str,
    id: &str,
    accept_lost: bool,
    options: &RunOptions,
) -> Result<DriveAdoption> {
    let journal = Journal::open(rclone, pool, id)?;
    let plan = journal
        .load_plan()?
        .ok_or_else(|| anyhow!("migration not found in the cloud: {id}"))?;
    if plan.pool != pool || plan.migration_id != id {
        bail!("journal plan does not belong to {pool}/{id}");
    }
    let drive = load_plan(&journal)?
        .ok_or_else(|| anyhow!("migration {id} has no drive part (it was planned without the drive, or the pool had none)"))?;
    check_target_unchanged(pool, &plan.target)?;
    let io = super::drive_adopt_live::LiveAdopt::new(rclone, &journal, &plan, options);
    adopt_core(
        &journal,
        &plan,
        &drive,
        &io,
        accept_lost,
        &super::execute::pc_id(),
    )
}

/// Adopted records are written for the plan's remotes; mounts use the saved
/// pool. Both must still agree.
fn check_target_unchanged(pool: &str, target: &PoolDefinition) -> Result<()> {
    let saved = crate::pool::load_pool_store()?
        .pools
        .get(pool)
        .cloned()
        .ok_or_else(|| anyhow!("pool not found: {pool}"))?;
    let set = |remotes: &[String]| -> Result<BTreeSet<String>> {
        Ok(crate::remote_root::apply_remote_roots(remotes.to_vec())?
            .into_iter()
            .collect())
    };
    if set(&saved.remotes)? != set(&target.remotes)?
        || saved.data_shards != target.data_shards
        || saved.parity_shards != target.parity_shards
        || saved.shard_size != target.shard_size
        || saved.native_crypt != target.native_crypt
    {
        bail!("the pool's saved settings changed since this migration was planned; create a new plan (nothing was changed)");
    }
    Ok(())
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
