//! Drive part of `pool migrate run` (phase 3): build a new archive for every
//! planned drive file that needs one, with the same resumable state machine
//! as archives (`execute::run_core`: claim, build, verified, switched; cloud
//! journal; leases; lost / unknown), keyed by `drive_model::entry_key`.
//! "Switched" means only that the new archive is ready: the drive itself
//! changes on adoption. The source manifests are read again from the cloud
//! (the plan stores none); a file changed since planning is left to the
//! adoption's catch-up. Starting the drive part freezes the source
//! generation (PCs on it stop publishing).
use super::drive_journal::{freeze, load_adoption, load_plan};
use super::drive_model::{entry_key, DriveEntry, DriveFile, DriveFreeze};
use super::drive_source::{CloudDrive, DriveSource};
use super::execute::{run_core, Effects, Replacement, RunOptions, RunSummary};
use super::journal::Journal;
use super::model::{Action, Entry, Plan, Record};
use crate::prelude::*;

/// Runs the drive part of `plan`, if it has one. `Ok(None)`: no drive part.
pub(crate) fn run(
    rclone: &str,
    journal: &Journal,
    plan: &Plan,
    options: &RunOptions,
    pc: &str,
) -> Result<Option<RunSummary>> {
    let Some(drive) = load_plan(journal)? else {
        return Ok(None);
    };
    if let Some(adoption) = load_adoption(journal)? {
        println!(
            "drive_adopted=true epoch={} (nothing left to run)",
            &adoption.epoch[..12]
        );
        return Ok(Some(RunSummary::default()));
    }
    let frozen = freeze(
        journal,
        &DriveFreeze {
            version: 1,
            migration_id: plan.migration_id.clone(),
            source: drive.source.clone(),
            epoch: drive.epoch.clone(),
            pc_id: pc.into(),
            ts_unix: crate::utils::now_unix(),
        },
    )?;
    println!(
        "drive_frozen={} since_unix={} (PCs on it stop publishing; their changes stay local)",
        frozen.source.label(),
        frozen.ts_unix
    );
    let source = CloudDrive::new(rclone, &plan.pool, &plan.target);
    let view = source.view(&drive.source)?;
    let current = by_key(view.files);
    let (planned, changed): (Vec<&DriveEntry>, Vec<&DriveEntry>) = drive
        .entries
        .iter()
        .partition(|e| current.contains_key(&e.key));
    // Files not checked at plan time are classified again by the adoption.
    let (unknown, planned): (Vec<&DriveEntry>, Vec<&DriveEntry>) = planned
        .into_iter()
        .partition(|e| e.action == Action::Unknown);
    println!(
        "drive_files planned={} changed_since_plan={} unknown_at_plan={} (both are caught up during adoption)",
        drive.entries.len(),
        changed.len(),
        unknown.len()
    );
    let entries = planned.into_iter().map(as_entry).collect();
    run_entries(
        rclone,
        journal,
        plan,
        drive.source.v7,
        entries,
        current,
        options,
        pc,
    )
    .map(Some)
}

/// Drive files by journal key.
pub(super) fn by_key(files: Vec<DriveFile>) -> BTreeMap<String, DriveFile> {
    files
        .into_iter()
        .map(|f| (entry_key(&f.path, &f.revision), f))
        .collect()
}

/// A drive entry in the archive state machine's terms.
pub(super) fn as_entry(entry: &DriveEntry) -> Entry {
    Entry {
        archive_id: entry.key.clone(),
        original_name: entry.path.clone(),
        size: entry.size,
        source: format!("drive:{}", entry.path),
        fingerprint: entry.fingerprint.clone(),
        action: entry.action,
        download_bytes: entry.download_bytes,
        upload_bytes: entry.upload_bytes,
        losses: entry.losses.clone(),
        detail: entry.detail.clone(),
    }
}

/// Runs `entries` (keys of `files`) through the archive state machine.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_entries(
    rclone: &str,
    journal: &Journal,
    plan: &Plan,
    v7: bool,
    entries: Vec<Entry>,
    files: BTreeMap<String, DriveFile>,
    options: &RunOptions,
    pc: &str,
) -> Result<RunSummary> {
    let work_root = crate::config::app_config_dir()?
        .join("migrations")
        .join(&plan.migration_id)
        .join("drive");
    fs::create_dir_all(&work_root)?;
    let effects = DriveEffects {
        rclone,
        journal,
        target: &plan.target,
        v7,
        files,
        work_root,
        stop_file: options.stop_file.clone(),
        take_over: options.take_over,
    };
    let work = Plan {
        entries,
        ..plan.clone()
    };
    run_core(&work, &journal.records()?, pc, &effects, options.parallel)
}

/// Adds the drive part's outcome to the archive run's summary.
pub(crate) fn merge(into: &mut RunSummary, other: RunSummary) {
    into.total += other.total;
    into.switched_now += other.switched_now;
    into.already_switched += other.already_switched;
    into.lost += other.lost;
    into.unknown.extend(other.unknown);
    into.claimed_elsewhere += other.claimed_elsewhere;
    into.stopped |= other.stopped;
}

/// Archive id of a new drive payload: v7 payloads must be privately owned.
pub(super) fn new_archive_id(v7: bool) -> Result<String> {
    Ok(if v7 {
        format!("peer-v7-{}", super::execute::random_hex(32)?)
    } else {
        format!("migrate-{}", super::execute::random_hex(12)?)
    })
}

struct DriveEffects<'a> {
    rclone: &'a str,
    journal: &'a Journal,
    target: &'a PoolDefinition,
    v7: bool,
    files: BTreeMap<String, DriveFile>,
    work_root: PathBuf,
    stop_file: Option<PathBuf>,
    take_over: bool,
}

impl DriveEffects<'_> {
    fn file(&self, entry: &Entry) -> Result<&DriveFile> {
        self.files
            .get(&entry.archive_id)
            .with_context(|| format!("{}: drive file changed since planning", entry.original_name))
    }
}

impl Effects for DriveEffects<'_> {
    fn append(&self, record: &Record) -> Result<()> {
        self.journal.append(record)
    }
    fn source_fingerprint(&self, entry: &Entry) -> Result<String> {
        crate::manifest::manifest_fingerprint(&self.file(entry)?.manifest)
    }
    fn build(&self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
        let file = self.file(entry)?;
        if crate::manifest::manifest_fingerprint(&file.manifest)? != entry.fingerprint {
            bail!("drive file payload changed since planning");
        }
        let work = self.work_root.join(new_archive_id);
        fs::create_dir_all(&work)?;
        let (produced, locations) = match entry.action {
            Action::Relocate => {
                let r = super::relocate::relocate(
                    self.rclone,
                    &file.manifest,
                    self.target,
                    new_archive_id,
                    &work,
                )?;
                println!(
                    "  relocated: downloaded {}, uploaded {} ({})",
                    crate::presentation::format_bytes(r.downloaded_bytes),
                    crate::presentation::format_bytes(r.uploaded_bytes),
                    entry.original_name
                );
                (r.manifest, r.manifest_locations)
            }
            Action::Reencode => crate::pool::reencode_manifest(
                self.rclone,
                &entry.source,
                &file.manifest,
                self.target,
                &work,
                new_archive_id,
            )?,
            other => bail!(
                "drive file {} is not movable ({other:?})",
                entry.original_name
            ),
        };
        if produced.archive_id != new_archive_id || produced.original_size != entry.size {
            bail!("replacement identity/size mismatch");
        }
        let new_manifest = locations
            .into_iter()
            .next()
            .context("replacement has no manifest replica")?;
        let _ = fs::remove_dir_all(&work);
        Ok(Replacement {
            new_archive_id: new_archive_id.to_string(),
            new_manifest,
        })
    }
    fn reverify(&self, entry: &Entry, new_archive_id: &str, new_manifest: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(self.rclone, new_manifest)?;
        crate::manifest::validate_manifest(&manifest)?;
        if manifest.archive_id != new_archive_id || manifest.original_size != entry.size {
            bail!("replacement manifest identity/size mismatch");
        }
        Ok(())
    }
    /// The drive switches on adoption, all files at once.
    fn switch(&self, _: &Entry, _: &Replacement) -> Result<()> {
        Ok(())
    }
    fn stop_requested(&self) -> bool {
        self.stop_file.as_deref().is_some_and(Path::exists)
    }
    fn take_over(&self) -> bool {
        self.take_over
    }
    fn new_archive_id(&self) -> Result<String> {
        new_archive_id(self.v7)
    }
}
