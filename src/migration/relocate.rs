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
//! rebuilt with Reed-Solomon.
//!
//! # Cheaper copies (phase 2)
//!
//! Copies use `rclone copyto` through the crypt remotes (`ShardCopier`), so
//! rclone encrypts the destination name for the new plaintext path:
//! - kept shard, same rclone remote, backend with server-side copy: the
//!   provider copies the ciphertext object as-is (no bytes through this PC).
//!   The copy stays valid: rclone crypt data carries its nonce in the object
//!   header and is keyed by the remote, not by the path; RPool's native crypt
//!   writes the same format. It is verified by equal ciphertext size and hash
//!   of source and destination on the crypt's base, or read back in full when
//!   the base reports no hash (or the hashes differ, e.g. rclone fell back to
//!   a streamed copy). Hash equality proves the copy is bit-identical to the
//!   source object; it does not re-authenticate the source itself (which the
//!   old archive keeps using either way) — `RPOOL_MIGRATE_FULL_SCAN=1` adds a
//!   full plaintext scan of the replacement;
//! - readable shard moving to another remote: streamed by rclone (download +
//!   upload, no temp file), then read back in full;
//! - group with an unreadable shard: `required` readable shards are
//!   downloaded, the rest is rebuilt and uploaded (read back by the write);
//!   kept readable shards of such a group are still copied server-side.
//!
//! Every shard of the replacement has a verification record before the
//! manifest is published; the old unconditional full scan is replaced by these.
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
use crate::storage::rclone::{remote_name, CryptCopyCapabilities, RcloneContext};
use crate::storage::traits::OperationContext;
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
    /// Shard bytes copied by the provider (server-side), not through this PC.
    pub server_side_bytes: u64,
    /// Bytes read back to verify new objects (not in `downloaded_bytes`).
    pub readback_bytes: u64,
    /// Shards verified by ciphertext hash instead of a readback.
    pub hash_verified_shards: usize,
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
///
/// `pinned` (rebalance) names the destination remote of some shards; the
/// others follow the placement rules.
pub(crate) fn relocate(
    rclone: &str,
    source: &Manifest,
    target: &PoolDefinition,
    pinned: &BTreeMap<u32, String>,
    new_archive_id: &str,
    work_dir: &Path,
) -> Result<Relocated> {
    let remotes = crate::remote_root::apply_remote_roots(target.remotes.clone())?;
    let domains = outage_domains(rclone, &remotes, target.placement)?;
    // RPOOL_MIGRATE_NO_COPY=1: phase 1 behaviour (download + upload), e.g. to
    // compare transfers or to avoid a provider whose server-side copy misbehaves.
    let copier = (!env_flag("RPOOL_MIGRATE_NO_COPY")).then(|| RcloneCopier::new(rclone));
    relocate_with_storage(
        &pool_writer(rclone, target),
        copier.as_ref().map(|c| c as &dyn ShardCopier),
        source,
        target,
        &remotes,
        &domains,
        pinned,
        new_archive_id,
        work_dir,
    )
}

/// Writes go through the pool's writer, exactly like `put`.
fn pool_writer(rclone: &str, target: &PoolDefinition) -> StorageWriter {
    StorageWriter::for_pool(rclone, target.native_crypt)
}

/// Forces the full readback scan of the replacement even when every shard
/// already has its own verification (hash or readback).
pub(crate) const FULL_SCAN_AFTER_RELOCATION: bool = false;

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"))
}

fn force_full_scan() -> bool {
    FULL_SCAN_AFTER_RELOCATION || env_flag("RPOOL_MIGRATE_FULL_SCAN")
}

/// Object copies that do not stage bytes on this PC.
pub(super) trait ShardCopier: Sync {
    /// Copies `source` to `destination`. Must refuse an existing destination
    /// (`StorageError::AlreadyExists`) and pass the crypt write gate.
    fn copy(&self, source: &str, destination: &str) -> Result<()>;
    /// True when the provider performs this copy (same rclone remote whose
    /// backend copies server-side). False when unknown.
    fn server_side(&self, source: &str, destination: &str) -> bool;
    /// (ciphertext size, `type:value` hash) of the stored object behind `address` on the
    /// crypt's base; None when the base reports no hash.
    fn stored_hash(&self, address: &str) -> Result<Option<(u64, String)>>;
}

/// Production copier: `rclone copyto` over the crypt remotes. Capabilities
/// are queried once per rclone remote name.
pub(super) struct RcloneCopier {
    context: RcloneContext,
    ctx: OperationContext,
    capabilities: Mutex<BTreeMap<String, Option<CryptCopyCapabilities>>>,
}

impl RcloneCopier {
    pub(super) fn new(rclone: &str) -> Self {
        Self {
            context: RcloneContext::inherited(rclone),
            ctx: OperationContext::none(),
            capabilities: Mutex::new(BTreeMap::new()),
        }
    }
    fn capabilities(&self, address: &str) -> Option<CryptCopyCapabilities> {
        let name = remote_name(address).ok()?.to_owned();
        if let Some(known) = self.capabilities.lock().ok()?.get(&name) {
            return known.clone();
        }
        let found = match self.context.crypt_copy_capabilities(&self.ctx, address) {
            Ok(found) => Some(found),
            Err(error) => {
                eprintln!("[relocate] copy capabilities of {name}: unknown ({error})");
                None
            }
        };
        self.capabilities.lock().ok()?.insert(name, found.clone());
        found
    }
}

impl ShardCopier for RcloneCopier {
    fn copy(&self, source: &str, destination: &str) -> Result<()> {
        Ok(self.context.copy_object(&self.ctx, source, destination)?)
    }
    fn server_side(&self, source: &str, destination: &str) -> bool {
        match (remote_name(source), remote_name(destination)) {
            (Ok(a), Ok(b)) if a == b => self
                .capabilities(source)
                .is_some_and(|c| c.server_side_copy),
            _ => false,
        }
    }
    fn stored_hash(&self, address: &str) -> Result<Option<(u64, String)>> {
        let Some(capabilities) = self.capabilities(address) else {
            return Ok(None);
        };
        if capabilities.hashes.is_empty() {
            return Ok(None);
        }
        let base = self
            .context
            .crypt_base_object(&self.ctx, address, &capabilities.base)?;
        Ok(self
            .context
            .object_hash(&self.ctx, &base, &capabilities.hashes)?)
    }
}

/// How a shard of the replacement was verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verified {
    /// Existed from an earlier attempt and was read back in full.
    Reused,
    /// Read back in full after it was written or copied.
    ReadBack,
    /// Provider copy with equal ciphertext size and hash.
    StoredHash,
}

/// Why a direct transfer failed: the source could not be read (the group
/// falls back to reconstruction) or the destination could not be written.
enum Failure {
    Source(anyhow::Error),
    Write(anyhow::Error),
}

fn is_already_exists(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::AlreadyExists)
}

/// Transfer primitives of one relocation, with byte counters.
struct Work<'a> {
    storage: &'a StorageWriter,
    reader: &'a crate::storage::reader::StorageReader,
    copier: Option<&'a dyn ShardCopier>,
    source: &'a Manifest,
    manifest: &'a Manifest,
    retries: u32,
    downloaded: AtomicU64,
    uploaded: AtomicU64,
    server_side: AtomicU64,
    readback: AtomicU64,
}

impl Work<'_> {
    fn counters(&self) -> (u64, u64, u64, u64) {
        (
            self.downloaded.load(Ordering::Relaxed),
            self.uploaded.load(Ordering::Relaxed),
            self.server_side.load(Ordering::Relaxed),
            self.readback.load(Ordering::Relaxed),
        )
    }
    fn server_side(&self, i: usize) -> bool {
        self.copier.is_some_and(|c| {
            c.server_side(
                &self.source.shards[i].object,
                &self.manifest.shards[i].object,
            )
        })
    }
    /// Verified download of source shard `i` into `dir`.
    fn download(&self, i: usize, dir: &Path) -> Result<PathBuf> {
        let shard = &self.source.shards[i];
        let path = dir.join(format!("source-{:08}.bin", shard.index));
        self.reader.download(shard, &path, self.retries, false)?;
        self.downloaded.fetch_add(shard.size, Ordering::Relaxed);
        Ok(path)
    }
    /// Upload of staged bytes; the write reads the object back in full.
    fn write_local(&self, i: usize, path: &Path) -> Result<Verified> {
        let shard = &self.manifest.shards[i];
        self.storage
            .write_file(path, 0, shard, self.retries.max(1))?;
        self.uploaded.fetch_add(shard.size, Ordering::Relaxed);
        self.readback.fetch_add(shard.size, Ordering::Relaxed);
        Ok(Verified::ReadBack)
    }
    /// Copy of source shard `i` to its new object, then verification.
    fn copy(&self, i: usize) -> Result<Verified> {
        let copier = self.copier.context("no object copier")?;
        let from = &self.source.shards[i];
        let to = &self.manifest.shards[i];
        let provider = copier.server_side(&from.object, &to.object);
        copier.copy(&from.object, &to.object)?;
        if provider {
            self.server_side.fetch_add(to.size, Ordering::Relaxed);
        } else {
            self.downloaded.fetch_add(to.size, Ordering::Relaxed);
            self.uploaded.fetch_add(to.size, Ordering::Relaxed);
        }
        if provider && self.same_ciphertext(copier, from, to) {
            return Ok(Verified::StoredHash);
        }
        self.reader.verify(to, true)?;
        self.readback.fetch_add(to.size, Ordering::Relaxed);
        Ok(Verified::ReadBack)
    }
    /// Equal ciphertext size and hash on the base, and the expected plaintext
    /// size through the crypt remote. Any doubt is `false` (read back instead).
    fn same_ciphertext(&self, copier: &dyn ShardCopier, from: &Shard, to: &Shard) -> bool {
        let hashes = copier
            .stored_hash(&from.object)
            .and_then(|a| Ok((a, copier.stored_hash(&to.object)?)));
        match hashes {
            Ok((Some(a), Some(b))) if a == b => self
                .reader
                .stat(&to.object)
                .is_ok_and(|m| m.size == to.size),
            Ok(_) => false,
            Err(error) => {
                eprintln!(
                    "[relocate] ciphertext hash of {} unavailable, reading back: {error:#}",
                    to.object
                );
                false
            }
        }
    }
    /// A pending, readable shard without reconstruction: copy it (server-side
    /// or streamed) when a copier exists, else download and upload it.
    fn transfer(&self, i: usize, dir: &Path) -> Result<Verified, Failure> {
        if self.copier.is_some() {
            match self.copy(i) {
                Ok(how) => return Ok(how),
                Err(error) if is_already_exists(&error) => return Err(Failure::Write(error)),
                Err(error) => eprintln!(
                    "[relocate] copy of shard {:08} failed, downloading instead: {error:#}",
                    self.source.shards[i].index
                ),
            }
        }
        let path = self.download(i, dir).map_err(Failure::Source)?;
        self.write_local(i, &path).map_err(Failure::Write)
    }
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
    let same_shard = crate::models::shard_size::shard_size_matches(
        source.shard_size,
        source.original_size,
        shard_bytes,
        target.data_shards,
        target.parity_shards,
    );
    if !same_coding || !same_shard {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn relocate_with_storage(
    storage: &StorageWriter,
    copier: Option<&dyn ShardCopier>,
    source: &Manifest,
    target: &PoolDefinition,
    remotes: &[String],
    domains: &[String],
    pinned: &BTreeMap<u32, String>,
    new_archive_id: &str,
    work_dir: &Path,
) -> Result<Relocated> {
    check_inputs(source, target, remotes, domains, new_archive_id)?;
    // A pinned shard is placed as if it already lived on its destination, so
    // the slot assignment keeps it there and the copy below moves it.
    let pin = |shard: &Shard| -> Result<Option<usize>> {
        pinned
            .get(&shard.index)
            .map(|remote| {
                remotes
                    .iter()
                    .position(|r| r == remote)
                    .with_context(|| format!("rebalance destination {remote} is not a pool remote"))
            })
            .transpose()
    };
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
        .map(|(shard, p)| {
            Ok(ReplacementSlot {
                group: shard.group,
                size: shard.size,
                current: match pin(shard)? {
                    Some(destination) => Some(destination),
                    None => p.on_target.filter(|_| p.problem.is_none()),
                },
            })
        })
        .collect::<Result<_>>()?;
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

    // 5. Build each group: copy what is readable (server-side when the
    //    provider can), rebuild what is not, write.
    fs::create_dir_all(work_dir)?;
    let work = Work {
        storage,
        reader,
        copier,
        source,
        manifest: &manifest,
        retries,
        downloaded: AtomicU64::new(0),
        uploaded: AtomicU64::new(0),
        server_side: AtomicU64::new(0),
        readback: AtomicU64::new(0),
    };
    let mut verified: BTreeMap<usize, Verified> = present_indexes
        .iter()
        .map(|&i| (i, Verified::Reused))
        .collect();
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
        // Direct transfers when every pending shard is readable.
        if !pending.iter().any(|i| problems.contains_key(i)) {
            let results: Vec<(usize, Result<Verified, Failure>)> = pool.install(|| {
                pending
                    .par_iter()
                    .map(|&i| (i, work.transfer(i, stage.path())))
                    .collect()
            });
            let mut first_error = None;
            for (i, result) in results {
                match result {
                    Ok(how) => {
                        verified.insert(i, how);
                        written.push(manifest.shards[i].object.clone());
                    }
                    Err(Failure::Source(error)) => {
                        let reason = reason_for_error(&error, probed[i].on_target.is_some());
                        eprintln!(
                            "[relocate] shard {:08} unreadable ({reason:?}): {error:#}",
                            source.shards[i].index
                        );
                        problems.insert(i, reason);
                    }
                    Err(Failure::Write(error)) => {
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
        }
        if let Some(loss) = group_loss(source, *group, members, *required, &problems) {
            return Err(failure(source, vec![loss], written));
        }
        let remaining: Vec<usize> = pending
            .iter()
            .copied()
            .filter(|i| !verified.contains_key(i))
            .collect();
        if remaining.is_empty() {
            continue;
        }
        // Reed-Solomon: fetch `required` readable shards, rebuild the others.
        let coding = source
            .coding
            .as_ref()
            .context("uncoded archive shard cannot be rebuilt")?;
        let mut candidates: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !problems.contains_key(i))
            .collect();
        // Shards that must pass through this PC anyway come first.
        candidates.sort_by_key(|&i| (!remaining.contains(&i) || work.server_side(i), i));
        let mut local: BTreeMap<usize, PathBuf> = BTreeMap::new();
        let mut next = 0usize;
        while local.len() < *required && next < candidates.len() {
            let take = (*required - local.len()).min(candidates.len() - next);
            let batch = &candidates[next..next + take];
            next += take;
            let fetched: Vec<(usize, Result<PathBuf>)> = pool.install(|| {
                batch
                    .par_iter()
                    .map(|&i| (i, work.download(i, stage.path())))
                    .collect()
            });
            for (i, result) in fetched {
                match result {
                    Ok(path) => {
                        local.insert(i, path);
                    }
                    Err(error) => {
                        let reason = reason_for_error(&error, probed[i].on_target.is_some());
                        eprintln!(
                            "[relocate] shard {:08} unreadable ({reason:?}): {error:#}",
                            source.shards[i].index
                        );
                        problems.insert(i, reason);
                    }
                }
            }
        }
        if let Some(loss) = group_loss(source, *group, members, *required, &problems) {
            return Err(failure(source, vec![loss], written));
        }
        if local.len() < *required {
            bail!("relocation could not stage enough shards of group {group}");
        }
        // The decoder treats every member not staged locally as missing.
        let missing: Vec<Shard> = members
            .iter()
            .filter(|i| !local.contains_key(i))
            .map(|&i| source.shards[i].clone())
            .collect();
        if !missing.is_empty() {
            let healthy: BTreeMap<u32, PathBuf> = local
                .iter()
                .map(|(i, path)| (source.shards[*i].index, path.clone()))
                .collect();
            let rebuilt =
                reconstruct_group_files(source, coding, *group, &missing, &healthy, stage.path())?;
            for shard in &missing {
                let path = rebuilt
                    .get(&shard.index)
                    .context("reconstruction did not produce a shard")?;
                local.insert(shard.index as usize, path.clone());
            }
        }
        let copyable = |i: usize| !problems.contains_key(&i) && work.server_side(i);
        let results: Vec<(usize, Result<Verified>)> = pool.install(|| {
            remaining
                .par_iter()
                .map(|&i| {
                    let result = local
                        .get(&i)
                        .context("relocation shard bytes are not staged")
                        .and_then(|path| {
                            if copyable(i) {
                                match work.copy(i) {
                                    Ok(how) => return Ok(how),
                                    Err(error) if is_already_exists(&error) => return Err(error),
                                    Err(error) => eprintln!(
                                        "[relocate] server-side copy of shard {:08} failed, uploading instead: {error:#}",
                                        source.shards[i].index
                                    ),
                                }
                            }
                            work.write_local(i, path)
                        });
                    (i, result)
                })
                .collect()
        });
        let mut first_error = None;
        for (i, result) in results {
            match result {
                Ok(how) => {
                    verified.insert(i, how);
                    written.push(manifest.shards[i].object.clone());
                }
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

    // 6. Every shard must be verified before the manifest is published: by a
    //    full readback (writes, streamed copies, reused objects) or, for a
    //    provider-side copy, by equal ciphertext size and hash of source and
    //    destination on the crypt's base. A full scan is forced by
    //    RPOOL_MIGRATE_FULL_SCAN=1 (or FULL_SCAN_AFTER_RELOCATION) and also
    //    covers any shard that somehow has no verification record.
    let unverified: Vec<usize> = (0..manifest.shards.len())
        .filter(|i| !verified.contains_key(i))
        .collect();
    if force_full_scan() || !unverified.is_empty() {
        let (report, _) = scan_manifest_with_storage(reader, &manifest, true, workers)?;
        let total: u64 = manifest.shards.iter().map(|s| s.size).sum();
        work.readback.fetch_add(total, Ordering::Relaxed);
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
    }
    let hash_verified_shards = verified
        .values()
        .filter(|v| **v == Verified::StoredHash)
        .count();

    let counters = work.counters();

    // 7. Publish the manifest replicas.
    let manifest_locations =
        replicate_manifest_with_storage(storage, &manifest, &publication, retries.max(1))?;
    if manifest_locations.is_empty() {
        bail!("relocated archive {new_archive_id} has no manifest replica");
    }
    let (downloaded_bytes, uploaded_bytes, server_side_bytes, readback_bytes) = counters;
    let relocated = Relocated {
        manifest,
        manifest_locations,
        downloaded_bytes,
        uploaded_bytes,
        server_side_bytes,
        readback_bytes,
        hash_verified_shards,
    };
    eprintln!(
        "[relocate] {} -> {new_archive_id}: downloaded={} uploaded={} server_side={} readback={} hash_verified={}/{} replicas={}",
        source.archive_id,
        relocated.downloaded_bytes,
        relocated.uploaded_bytes,
        relocated.server_side_bytes,
        relocated.readback_bytes,
        relocated.hash_verified_shards,
        relocated.manifest.shards.len(),
        relocated.manifest_locations.len()
    );
    Ok(relocated)
}

#[cfg(test)]
#[path = "relocate_tests.rs"]
mod tests;
