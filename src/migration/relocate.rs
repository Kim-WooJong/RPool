//! Relocation (work package C): rebuild an archive whose coding is unchanged
//! but whose shards sit (partly) on remotes that left the pool, as a NEW
//! archive on the new policy, moving as little as possible. The original
//! archive is never modified or deleted.
//!
//! # Copy, not borrow (phase 1)
//!
//! Every shard of the replacement is written under the new archive's own
//! object paths (`<remote>/<new_archive_id>/...`); the new manifest never
//! references an object of the old archive. Borrowing the old objects would
//! pass `validate_manifest`, but it is unsafe elsewhere today:
//! - `provider drain --delete-source` deletes the old manifest's shard objects;
//! - drive retention (`mount/retention.rs`) deletes the exact objects of old
//!   owned `virtual-*` archives and rejects owned manifests whose objects lie
//!   outside their own archive directory;
//! - reprocess switching rejects replacements with borrowed objects;
//! - repair of the new archive would write into the old archive's namespace.
//!
//! "Minimal movement" therefore means minimal *placement* change: a healthy
//! shard on a remote that stays in the pool is copied verbatim to the new path
//! on the SAME remote; only shards on removed/failed remotes (or that break
//! the outage bound) go to another remote. Only shards that cannot be read are
//! rebuilt with Reed-Solomon. The storage layer has no guaranteed server-side
//! copy yet, so a same-remote copy is still a download plus an upload.
use super::model::{GroupLoss, MissingReason, MissingShard};
use crate::maintenance::{reconstruct_group_files, scan_manifest_with_storage};
use crate::manifest::{
    coding_group_count, content_root_v2, data_shards, publication_remotes,
    replicate_manifest_with_storage, validate_manifest,
};
use crate::placement::{assign_replacement_slots, outage_domains, ReplacementSlot};
use crate::planning::{manifest_single_provider_failure_safety, FailureSafety};
use crate::prelude::*;
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::storage::writer::StorageWriter;
use crate::utils::remote_join;
use std::sync::atomic::{AtomicU64, Ordering};

/// Result of relocating one archive.
#[derive(Debug, Clone)]
pub(crate) struct Relocated {
    /// The verified replacement manifest (new archive id).
    pub manifest: Manifest,
    /// Where its manifest replicas were written, `remote:path/manifest.json`.
    pub manifest_locations: Vec<String>,
    /// Source shard bytes fetched (verbatim copies and reconstruction inputs).
    /// Readback verification of the new objects is not included.
    pub downloaded_bytes: u64,
    /// New shard bytes written (manifest replicas not included; objects that
    /// already held the exact content from an earlier attempt are not counted).
    pub uploaded_bytes: u64,
}

/// A group has fewer than K readable shards and every missing shard is a
/// definite loss (missing, corrupt, bad size, or on a removed remote). The
/// orchestrator owns the type; the migration records the archive as lost.
pub(crate) use super::execute::Unrecoverable;

fn unrecoverable(losses: Vec<GroupLoss>) -> anyhow::Error {
    Unrecoverable(losses).into()
}

/// A group lacks K readable shards but a provider error on a pool remote hides
/// at least one of them: retry later, never treat it as lost. The `Display`
/// text starts with the stable marker `UnknownGroup`.
#[derive(Debug, Clone)]
pub(crate) struct RelocateError {
    pub archive_id: String,
    pub losses: Vec<GroupLoss>,
    /// Objects already written under the new archive id before the problem was
    /// discovered (only possible when a shard fails during download after the
    /// quick probe passed). No manifest references them.
    pub orphans: Vec<String>,
}

fn write_losses(f: &mut std::fmt::Formatter<'_>, losses: &[GroupLoss]) -> std::fmt::Result {
    for loss in losses {
        write!(
            f,
            " group {} has {}/{} shards",
            loss.group, loss.available, loss.required_k
        )?;
    }
    Ok(())
}

impl std::fmt::Display for RelocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "UnknownGroup: archive {} cannot be relocated now (provider error);",
            self.archive_id
        )?;
        write_losses(f, &self.losses)?;
        if !self.orphans.is_empty() {
            write!(f, "; {} orphan object(s) written", self.orphans.len())?;
        }
        Ok(())
    }
}

impl std::error::Error for RelocateError {}

/// Rebuilds `source` (a valid manifest of the pool) for `target`. Shards that
/// are readable on remotes still in `target` may be reused; shards on removed
/// or failed remotes are reconstructed (Reed-Solomon) or copied verbatim from
/// a still-readable removed remote, and written to `target` remotes under the
/// placement rules. The replacement is fully read back and verified before
/// returning. `work_dir` holds temporary data.
pub(crate) fn relocate(
    rclone: &str,
    source: &Manifest,
    target: &PoolDefinition,
    new_archive_id: &str,
    work_dir: &Path,
) -> Result<Relocated> {
    let remotes = crate::remote_root::apply_remote_roots(target.remotes.clone())?;
    let domains = outage_domains(rclone, &remotes, target.placement)?;
    relocate_with_storage(
        &pool_writer(rclone, target),
        source,
        target,
        &remotes,
        &domains,
        new_archive_id,
        work_dir,
    )
}

/// Writes go through the pool's writer, exactly like `put`.
fn pool_writer(rclone: &str, target: &PoolDefinition) -> StorageWriter {
    StorageWriter::for_pool(rclone, target.native_crypt)
}

/// One source shard after the quick probe.
struct Probed {
    /// Index into the resolved target remotes when the shard's remote stays.
    on_target: Option<usize>,
    /// `None` when the size probe passed.
    problem: Option<MissingReason>,
}

fn reason_for(probe: &Probe, on_target: bool) -> Option<MissingReason> {
    match probe {
        Probe::Ok => None,
        Probe::Missing => Some(MissingReason::Missing),
        Probe::BadSize { .. } => Some(MissingReason::BadSize),
        Probe::Corrupt { .. } => Some(MissingReason::Corrupt),
        Probe::Error(_) if !on_target => Some(MissingReason::RemoteRemoved),
        Probe::Error(_) => Some(MissingReason::ProviderError),
    }
}

fn reason_for_error(error: &anyhow::Error, on_target: bool) -> MissingReason {
    match error.downcast_ref::<StorageError>().map(StorageError::kind) {
        Some(StorageErrorKind::NotFound) => MissingReason::Missing,
        Some(StorageErrorKind::CorruptData) => MissingReason::Corrupt,
        _ if !on_target => MissingReason::RemoteRemoved,
        _ => MissingReason::ProviderError,
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::NotFound)
}

/// Relative object path of `shard` in archive `archive_id`, as `put` names it.
fn relative_object(archive_id: &str, shard: &Shard, coding: Option<&Coding>) -> String {
    match (coding, shard.kind) {
        (None, _) => format!("{archive_id}/shards/{:08}.bin", shard.index),
        (Some(_), ShardKind::Data) => format!("{archive_id}/data/{:08}.bin", shard.index),
        (Some(coding), ShardKind::Parity) => format!(
            "{archive_id}/parity/g{:08}-p{:03}.bin",
            shard.group,
            (shard.slot as usize).saturating_sub(coding.data_shards)
        ),
    }
}

fn check_inputs(
    source: &Manifest,
    target: &PoolDefinition,
    remotes: &[String],
    domains: &[String],
    new_archive_id: &str,
) -> Result<()> {
    if source.version != 2 {
        bail!("relocation requires a v2 manifest");
    }
    validate_manifest(source)?;
    if new_archive_id.is_empty()
        || new_archive_id == source.archive_id
        || new_archive_id.starts_with('.')
        || !new_archive_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        bail!("relocation needs a fresh, path-safe archive id different from the source");
    }
    if remotes.is_empty() || remotes.len() != domains.len() {
        bail!("relocation target has no remotes");
    }
    let unique: BTreeSet<&String> = remotes.iter().collect();
    if unique.len() != remotes.len() {
        bail!("relocation target lists a remote more than once");
    }
    let shard_bytes = target.shard_bytes()?.get();
    let same_coding = match &source.coding {
        None => target.parity_shards == 0,
        Some(coding) => {
            coding.data_shards == target.data_shards && coding.parity_shards == target.parity_shards
        }
    };
    if !same_coding || shard_bytes != source.shard_size {
        bail!("relocation requires unchanged coding (K, M, shard size); re-encode instead");
    }
    crate::models::shard_size::check_object_limit(source.shard_size, target.max_object_bytes)
}

/// Groups of the archive: (group id, shard positions, required readable).
fn groups_of(source: &Manifest) -> Vec<(u32, Vec<usize>, usize)> {
    let Some(coding) = &source.coding else {
        // Uncoded archives: nothing can be rebuilt; every shard is required.
        return vec![(0, (0..source.shards.len()).collect(), source.shards.len())];
    };
    let data = data_shards(source);
    (0..coding_group_count(data.len(), coding.data_shards))
        .map(|group| {
            let group = group as u32;
            let members: Vec<usize> = (0..source.shards.len())
                .filter(|&i| source.shards[i].group == group)
                .collect();
            let real = members
                .iter()
                .filter(|&&i| source.shards[i].kind == ShardKind::Data)
                .count();
            // Virtual zero data shards of a short final group are always known.
            let required = coding.data_shards - coding.data_shards.saturating_sub(real);
            (group, members, required)
        })
        .collect()
}

fn group_loss(
    source: &Manifest,
    group: u32,
    members: &[usize],
    required: usize,
    problems: &BTreeMap<usize, MissingReason>,
) -> Option<GroupLoss> {
    let missing: Vec<MissingShard> = members
        .iter()
        .filter_map(|i| {
            problems.get(i).map(|reason| MissingShard {
                index: source.shards[*i].index,
                remote: source.shards[*i].remote.clone(),
                reason: *reason,
            })
        })
        .collect();
    let available = members.len() - missing.len();
    (available < required).then(|| GroupLoss {
        group,
        required_k: source
            .coding
            .as_ref()
            .map_or(members.len(), |c| c.data_shards),
        available: available
            + source
                .coding
                .as_ref()
                .map_or(0, |c| c.data_shards - required),
        missing,
    })
}

fn failure(source: &Manifest, losses: Vec<GroupLoss>, orphans: Vec<String>) -> anyhow::Error {
    let unknown = losses.iter().any(|loss| {
        loss.missing
            .iter()
            .any(|m| m.reason == MissingReason::ProviderError)
    });
    if unknown {
        return RelocateError {
            archive_id: source.archive_id.clone(),
            losses,
            orphans,
        }
        .into();
    }
    for orphan in &orphans {
        eprintln!("[relocate] orphan object (no manifest references it): {orphan}");
    }
    unrecoverable(losses)
}

pub(super) fn relocate_with_storage(
    storage: &StorageWriter,
    source: &Manifest,
    target: &PoolDefinition,
    remotes: &[String],
    domains: &[String],
    new_archive_id: &str,
    work_dir: &Path,
) -> Result<Relocated> {
    check_inputs(source, target, remotes, domains, new_archive_id)?;
    let workers = target.workers.max(1);
    let retries = target.retries;
    let reader = storage.reader();
    for remote in remotes {
        storage.ensure_destination(remote)?;
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()?;

    // 1. Quick probe of every source shard (size only; content is verified on download).
    let probed: Vec<Probed> = pool.install(|| {
        source
            .shards
            .par_iter()
            .map(|shard| {
                let on_target = remotes.iter().position(|r| r == &shard.remote);
                let probe = reader.probe(shard, false);
                Probed {
                    on_target,
                    problem: reason_for(&probe, on_target.is_some()),
                }
            })
            .collect()
    });
    let mut problems: BTreeMap<usize, MissingReason> = probed
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.problem.map(|reason| (i, reason)))
        .collect();

    // 2. Refuse before writing anything when a group is short.
    let groups = groups_of(source);
    let losses: Vec<GroupLoss> = groups
        .iter()
        .filter_map(|(group, members, required)| {
            group_loss(source, *group, members, *required, &problems)
        })
        .collect();
    if !losses.is_empty() {
        return Err(failure(source, losses, Vec::new()));
    }

    // 3. Placement: keep healthy shards on their remote, move the rest.
    let slots: Vec<ReplacementSlot> = source
        .shards
        .iter()
        .zip(&probed)
        .map(|(shard, p)| ReplacementSlot {
            group: shard.group,
            size: shard.size,
            current: p.on_target.filter(|_| p.problem.is_none()),
        })
        .collect();
    let (bound, strict) = match &source.coding {
        Some(coding) => (
            coding.parity_shards,
            target.placement == Placement::Resilient,
        ),
        None => (usize::MAX, false),
    };
    let assigned = assign_replacement_slots(domains, &slots, bound, strict)?;

    let source_objects: BTreeSet<&str> = source.shards.iter().map(|s| s.object.as_str()).collect();
    let mut shards = Vec::with_capacity(source.shards.len());
    for (shard, index) in source.shards.iter().zip(&assigned) {
        let remote = &remotes[*index];
        let mut moved = shard.clone();
        moved.remote = remote.clone();
        moved.object = remote_join(
            remote,
            &relative_object(new_archive_id, shard, source.coding.as_ref()),
        );
        if source_objects.contains(moved.object.as_str()) {
            bail!("relocation destination {} is a source object", moved.object);
        }
        shards.push(moved);
    }
    let manifest = Manifest {
        version: 2,
        archive_id: new_archive_id.to_owned(),
        original_name: source.original_name.clone(),
        original_size: source.original_size,
        shard_size: source.shard_size,
        // Deterministic, so a retry with the same id reproduces identical bytes.
        created_unix: source.created_unix,
        content_root_blake3: content_root_v2(
            source.original_size,
            source.shard_size,
            &source.coding,
            &shards,
        ),
        coding: source.coding.clone(),
        shards,
    };
    validate_manifest(&manifest)?;
    if let Some(coding) = &manifest.coding {
        let (safety, max) = manifest_single_provider_failure_safety(&manifest, coding);
        if safety == FailureSafety::Unsafe {
            eprintln!(
                "[warning] relocated archive keeps up to {max} shard(s) of one group on a configured remote (parity={}); add remotes for single-provider failure safety",
                coding.parity_shards
            );
        }
    }
    let publication = publication_remotes(&manifest, remotes, target.placement);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    // 4. Never overwrite foreign data: an existing destination must already
    //    hold exactly the expected content (an earlier attempt of this id).
    let present: Vec<Result<bool>> = pool.install(|| {
        manifest
            .shards
            .par_iter()
            .map(|shard| match reader.stat(&shard.object) {
                Ok(_) => match reader.verify(shard, true) {
                    Ok(()) => Ok(true),
                    Err(error) => Err(error.context(format!(
                        "relocation destination {} already exists with other content",
                        shard.object
                    ))),
                },
                Err(error) if is_not_found(&error) => Ok(false),
                Err(error) => Err(error),
            })
            .collect()
    });
    let mut present_indexes = BTreeSet::new();
    for (position, result) in present.into_iter().enumerate() {
        if result? {
            present_indexes.insert(position);
        }
    }
    for remote in &publication {
        let path = remote_join(remote, &format!("{new_archive_id}/manifest.json"));
        match reader.read_metadata(&path) {
            Ok(bytes) if bytes == manifest_bytes => {}
            Ok(_) => bail!("relocation manifest {path} already exists with other content"),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }

    // 5. Build each group: fetch, rebuild what is unreadable, write.
    fs::create_dir_all(work_dir)?;
    let downloaded = AtomicU64::new(0);
    let uploaded = AtomicU64::new(0);
    let mut written: Vec<String> = Vec::new();
    for (group, members, required) in &groups {
        let pending: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !present_indexes.contains(i))
            .collect();
        if pending.is_empty() {
            continue;
        }
        let stage = tempfile::Builder::new()
            .prefix("relocate-")
            .tempdir_in(work_dir)?;
        // Reconstruction needs every other shard of the group locally.
        let rebuild = pending.iter().any(|i| problems.contains_key(i));
        let fetch: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !problems.contains_key(i) && (rebuild || pending.contains(i)))
            .collect();
        let fetched: Vec<(usize, Result<PathBuf>)> = pool.install(|| {
            fetch
                .par_iter()
                .map(|&i| {
                    let shard = &source.shards[i];
                    let path = stage.path().join(format!("source-{:08}.bin", shard.index));
                    let result = reader.download(shard, &path, retries, false).map(|()| {
                        downloaded.fetch_add(shard.size, Ordering::Relaxed);
                        path
                    });
                    (i, result)
                })
                .collect()
        });
        let mut local: BTreeMap<usize, PathBuf> = BTreeMap::new();
        let mut failed_download = false;
        for (i, result) in fetched {
            match result {
                Ok(path) => {
                    local.insert(i, path);
                }
                Err(error) => {
                    failed_download = true;
                    let reason = reason_for_error(&error, probed[i].on_target.is_some());
                    eprintln!(
                        "[relocate] shard {:08} unreadable ({reason:?}): {error:#}",
                        source.shards[i].index
                    );
                    problems.insert(i, reason);
                }
            }
        }
        if let Some(loss) = group_loss(source, *group, members, *required, &problems) {
            return Err(failure(source, vec![loss], written));
        }
        if failed_download && !pending.iter().all(|i| local.contains_key(i)) {
            // A late failure turned a copy into a rebuild: fetch the rest of the group.
            for &i in members {
                if local.contains_key(&i) || problems.contains_key(&i) {
                    continue;
                }
                let shard = &source.shards[i];
                let path = stage.path().join(format!("source-{:08}.bin", shard.index));
                match reader.download(shard, &path, retries, false) {
                    Ok(()) => {
                        downloaded.fetch_add(shard.size, Ordering::Relaxed);
                        local.insert(i, path);
                    }
                    Err(error) => {
                        problems.insert(i, reason_for_error(&error, probed[i].on_target.is_some()));
                    }
                }
            }
            if let Some(loss) = group_loss(source, *group, members, *required, &problems) {
                return Err(failure(source, vec![loss], written));
            }
        }
        if pending.iter().any(|i| !local.contains_key(i)) {
            // Every group member not staged locally is rebuilt (the decoder
            // needs all others); only the pending ones are written.
            let missing: Vec<Shard> = members
                .iter()
                .filter(|i| !local.contains_key(i))
                .map(|&i| source.shards[i].clone())
                .collect();
            let coding = source
                .coding
                .as_ref()
                .context("uncoded archive shard cannot be rebuilt")?;
            let healthy: BTreeMap<u32, PathBuf> = local
                .iter()
                .map(|(i, path)| (source.shards[*i].index, path.clone()))
                .collect();
            let rebuilt =
                reconstruct_group_files(source, coding, *group, &missing, &healthy, stage.path())?;
            for shard in &missing {
                let position = shard.index as usize;
                let path = rebuilt
                    .get(&shard.index)
                    .context("reconstruction did not produce a shard")?;
                local.insert(position, path.clone());
            }
        }
        let results: Vec<(usize, Result<()>)> = pool.install(|| {
            pending
                .par_iter()
                .map(|&i| {
                    let shard = &manifest.shards[i];
                    let result = local
                        .get(&i)
                        .context("relocation shard bytes are not staged")
                        .and_then(|path| storage.write_file(path, 0, shard, retries.max(1)))
                        .map(|()| {
                            uploaded.fetch_add(shard.size, Ordering::Relaxed);
                        });
                    (i, result)
                })
                .collect()
        });
        let mut first_error = None;
        for (i, result) in results {
            match result {
                Ok(()) => written.push(manifest.shards[i].object.clone()),
                Err(error) => {
                    first_error.get_or_insert(error.context(format!(
                        "cannot write relocated shard {}",
                        manifest.shards[i].object
                    )));
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        drop(stage);
    }

    // 6. Full readback of the whole replacement before publishing it.
    let (report, _) = scan_manifest_with_storage(reader, &manifest, true, workers)?;
    if report.healthy != report.total {
        bail!(
            "relocated archive {new_archive_id} failed full verification: healthy={}/{} missing={} bad_size={} corrupt={} errors={}",
            report.healthy,
            report.total,
            report.missing,
            report.bad_size,
            report.corrupt,
            report.errors
        );
    }

    // 7. Publish the manifest replicas.
    let manifest_locations =
        replicate_manifest_with_storage(storage, &manifest, &publication, retries.max(1))?;
    if manifest_locations.is_empty() {
        bail!("relocated archive {new_archive_id} has no manifest replica");
    }
    let downloaded_bytes = downloaded.into_inner();
    let uploaded_bytes = uploaded.into_inner();
    eprintln!(
        "[relocate] {} -> {new_archive_id}: downloaded={downloaded_bytes} uploaded={uploaded_bytes} replicas={}",
        source.archive_id,
        manifest_locations.len()
    );
    Ok(Relocated {
        manifest,
        manifest_locations,
        downloaded_bytes,
        uploaded_bytes,
    })
}

#[cfg(test)]
#[path = "relocate_tests.rs"]
mod tests;
