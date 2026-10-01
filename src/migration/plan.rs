//! Planner (work package A): classify every archive of a pool against its
//! saved policy, estimate bytes and time, and list unrecoverable archives.
use super::classify::{reencode_reason, shards_to_move};
use super::enumerate::{
    enumerate, parse_lsjson, Cloud, CopyFeatures, Found, Loaded, RemoteListing,
};
use super::estimate::{reencode_transfer, relocate_transfer, Transfer};
use super::model::{Action, Counts, Entry, Plan};
use super::probe::{assess, full_states, quick_states, ShardState};
use crate::prelude::*;
use crate::storage::admin::{BackendAdmin, RcloneAdmin, RemoteCatalog};
use crate::storage::rclone::RcloneContext;
use crate::storage::reader::StorageReader;
use crate::storage::traits::OperationContext;

#[derive(Debug, Clone, Default)]
pub(crate) struct PlanOptions {
    /// Hash every shard of affected archives instead of listing sizes.
    pub probe_full: bool,
    pub download_mib_s: Option<f64>,
    pub upload_mib_s: Option<f64>,
    pub workers: usize,
    /// Leave the pool's drive out (archives only). The drive is planned by
    /// default when the pool has one.
    pub skip_drive: bool,
}

/// Builds a plan for `pool` (the saved pool is the target policy) with the
/// drive part (unless `options.skip_drive`): the drive's files read from the
/// cloud, classified against the same listings. Read-only: nothing is
/// written anywhere. Blocking; may take a while.
pub(crate) fn plan_with_drive(
    rclone: &str,
    pool: &str,
    options: &PlanOptions,
) -> Result<(Plan, Option<super::drive_model::DrivePlan>)> {
    let store = crate::pool::load_pool_store()?;
    let mut target = store
        .pools
        .get(pool)
        .cloned()
        .ok_or_else(|| anyhow!("pool not found: {pool}"))?;
    crate::pool::validate_pool(&target)?;
    target.remotes = crate::remote_root::apply_remote_roots(target.remotes)?;
    let inventory = crate::inventory::load_inventory()?;
    let cloud = RcloneCloud::new(rclone, target.placement == Placement::Resilient);
    let (replaced, mut note) = replaced_archives(rclone, pool);
    let (mut plan, mut listed) = plan_listed(&cloud, pool, target, &inventory, options, &replaced)?;
    plan.notes.append(&mut note);
    if options.skip_drive {
        return Ok((plan, None));
    }
    let source = super::drive_source::CloudDrive::new(rclone, pool, &plan.target);
    let drive = super::drive_plan::plan_drive(&cloud, &source, &mut plan, &mut listed, options)?;
    Ok((plan, drive))
}

/// Originals that an earlier migration of this pool already replaced
/// (`Switched` in its cloud journal). Their replacement is what the drive and
/// inventory use, so they are neither moved again nor reported as lost.
/// Read-only; an unreadable journal only adds a note.
fn replaced_archives(rclone: &str, pool: &str) -> (BTreeSet<String>, Vec<String>) {
    let mut replaced = BTreeSet::new();
    let mut notes = Vec::new();
    let ids = match super::journal::discover(rclone, pool) {
        Ok(ids) => ids,
        Err(e) => {
            notes.push(format!("earlier migrations could not be read ({e:#}); already replaced originals may be planned again"));
            return (replaced, notes);
        }
    };
    for id in ids {
        let records = super::journal::Journal::open(rclone, pool, &id).and_then(|j| j.records());
        match records {
            Ok(records) => {
                for (archive, state) in super::execute::progress(&records) {
                    // Drive records are keyed by file, not by archive.
                    if super::drive_model::is_drive_key(&archive) {
                        continue;
                    }
                    if matches!(state, super::execute::Progress::Switched(_)) {
                        replaced.insert(archive);
                    }
                }
            }
            Err(e) => notes.push(format!("migration {id} could not be read ({e:#})")),
        }
    }
    (replaced, notes)
}

/// Production cloud access: rclone listings, metadata reads, scans and quota
/// queries only.
pub(super) struct RcloneCloud {
    context: RcloneContext,
    reader: StorageReader,
    admin: RcloneAdmin,
    catalog: Option<RemoteCatalog>,
    /// Copy features per rclone remote name, queried once per plan.
    features: Mutex<BTreeMap<String, Option<CopyFeatures>>>,
}

impl RcloneCloud {
    pub(super) fn new(rclone: &str, need_catalog: bool) -> Self {
        let admin = RcloneAdmin::inherited(rclone);
        let catalog = need_catalog.then(|| admin.catalog().ok()).flatten();
        Self {
            context: RcloneContext::inherited(rclone),
            reader: StorageReader::rclone(rclone),
            admin,
            catalog,
            features: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Cloud for RcloneCloud {
    fn configured(&self) -> Option<BTreeSet<String>> {
        let names = self.admin.discover().ok()?;
        Some(
            names
                .into_iter()
                .map(|n| n.trim_end_matches(':').to_owned())
                .collect(),
        )
    }
    fn list(&self, remote: &str) -> RemoteListing {
        match self
            .context
            .list_recursive(&OperationContext::none(), remote)
        {
            Ok(bytes) => match parse_lsjson(&bytes) {
                Ok(files) => RemoteListing::Listed(files),
                Err(error) => RemoteListing::Failed(format!("{error:#}")),
            },
            Err(error) if error.kind() == crate::storage::error::StorageErrorKind::NotFound => {
                RemoteListing::Listed(BTreeMap::new())
            }
            Err(error) => RemoteListing::Failed(error.to_string()),
        }
    }
    fn read(&self, address: &str) -> Result<Vec<u8>> {
        crate::manifest::load_manifest_bytes_with_storage(&self.reader, address)
    }
    fn probe_full(&self, manifest: &Manifest, workers: usize) -> Result<Vec<(Shard, Probe)>> {
        crate::maintenance::scan_manifest_with_storage(&self.reader, manifest, true, workers)
            .map(|(_, probes)| probes)
    }
    fn failure_domain(&self, remote: &str) -> Option<String> {
        let binding = self.catalog.as_ref()?.capacity(remote).ok()?;
        binding.failure_domain.map(|d| d.as_str().to_owned())
    }
    fn quota_ok(&self, target: &PoolDefinition, specs: &[Vec<PhysicalSpec>]) -> Option<bool> {
        super::estimate::quota_ok(&self.admin, target, specs)
    }
    /// `rclone backend features` of the crypt remote (Copy) and of its base
    /// (Hashes). Errors (including non-crypt remotes) mean unknown.
    fn copy_features(&self, remote: &str) -> Option<CopyFeatures> {
        let name = remote_name(remote).to_owned();
        if let Some(known) = self.features.lock().ok()?.get(&name) {
            return *known;
        }
        let found = self
            .context
            .crypt_copy_capabilities(&OperationContext::none(), &format!("{name}:"))
            .ok()
            .map(|c| CopyFeatures {
                server_side_copy: c.server_side_copy,
                ciphertext_hash: !c.hashes.is_empty(),
            });
        self.features.lock().ok()?.insert(name, found);
        found
    }
}

fn remote_name(remote: &str) -> &str {
    remote.split_once(':').map_or(remote, |(name, _)| name)
}

/// One listing per remote, in parallel; unconfigured remotes are not called.
pub(super) fn list_all(
    cloud: &dyn Cloud,
    remotes: &BTreeSet<String>,
    configured: Option<&BTreeSet<String>>,
) -> BTreeMap<String, RemoteListing> {
    remotes
        .par_iter()
        .map(|remote| {
            let listing = match configured {
                Some(names) if !names.contains(remote_name(remote)) => RemoteListing::NotConfigured,
                _ => cloud.list(remote),
            };
            (remote.clone(), listing)
        })
        .collect()
}

fn pc_name() -> String {
    for key in ["RPOOL_PC_NAME", "COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return value.trim().to_owned();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-pc".into())
}

fn random_hex32() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random identifier: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn first_unknown(states: &[ShardState]) -> Option<&str> {
    states.iter().find_map(|s| match s {
        ShardState::Unknown(e) => Some(e.as_str()),
        _ => None,
    })
}

/// Classifies, probes and estimates one archive. Returns the entry and the
/// new shards to charge against quota.
pub(super) fn plan_entry(
    cloud: &dyn Cloud,
    loaded: &Loaded,
    target: &PoolDefinition,
    target_set: &BTreeSet<String>,
    listings: &BTreeMap<String, RemoteListing>,
    options: &PlanOptions,
    workers: usize,
) -> Result<(Entry, Transfer)> {
    let manifest = &loaded.manifest;
    let reencode = reencode_reason(manifest, target);
    let (moving, move_reasons) =
        shards_to_move(manifest, target, target_set, &|r| cloud.failure_domain(r));
    let base = if reencode.is_some() {
        Action::Reencode
    } else if !moving.is_empty() {
        Action::Relocate
    } else {
        Action::Unaffected
    };
    let mut notes: Vec<String> = reencode.into_iter().chain(move_reasons).collect();
    let mut states = quick_states(manifest, listings, target_set);
    if options.probe_full && base != Action::Unaffected {
        match cloud.probe_full(manifest, workers) {
            Ok(probes) => states = full_states(manifest, &probes, target_set),
            Err(error) => notes.push(format!("full probe failed, sizes only: {error:#}")),
        }
    }
    let availability = assess(manifest, &states);
    let action = if availability.lost {
        Action::Lost
    } else if availability.undetermined {
        Action::Unknown
    } else {
        base
    };
    let planned = match (action, base) {
        (Action::Lost, _) | (_, Action::Unaffected) => Ok(Transfer::default()),
        (_, Action::Relocate) => {
            relocate_transfer(manifest, &states, &moving, &|r| cloud.copy_features(r))
        }
        (_, _) => reencode_transfer(manifest, target),
    };
    let transfer = match planned {
        Ok(transfer) => transfer,
        Err(error) => {
            notes.push(format!("estimate unavailable: {error:#}"));
            Transfer::default()
        }
    };
    let degraded = availability.losses.len();
    let losses = match action {
        Action::Unaffected => {
            if degraded > 0 {
                notes.push(format!(
                    "{degraded} group(s) degraded on pool remotes; run a scrub and repair"
                ));
            }
            Vec::new()
        }
        _ => availability.losses,
    };
    match action {
        Action::Unknown => notes.insert(
            0,
            format!(
                "could not be checked (planned: {base:?}); provider error: {}",
                first_unknown(&states).unwrap_or("unknown")
            ),
        ),
        Action::Lost => notes.insert(0, "some group has fewer than K readable shards".into()),
        _ => {}
    }
    let entry = Entry {
        archive_id: manifest.archive_id.clone(),
        original_name: manifest.original_name.clone(),
        size: manifest.original_size,
        source: loaded.source.clone(),
        fingerprint: loaded.fingerprint.clone(),
        action,
        download_bytes: transfer.download,
        upload_bytes: transfer.upload,
        losses,
        detail: (!notes.is_empty()).then(|| notes.join("; ")),
    };
    Ok((entry, transfer))
}

/// Planner core over an abstract cloud. `target.remotes` must be resolved.
#[cfg(test)]
pub(crate) fn plan_with(
    cloud: &dyn Cloud,
    pool: &str,
    target: PoolDefinition,
    inventory: &InventoryStore,
    options: &PlanOptions,
) -> Result<Plan> {
    plan_with_replaced(cloud, pool, target, inventory, options, &BTreeSet::new())
}

/// `plan_with`, skipping archives in `replaced` (already migrated originals).
/// Production planning uses [`plan_listed`].
#[cfg(test)]
pub(crate) fn plan_with_replaced(
    cloud: &dyn Cloud,
    pool: &str,
    target: PoolDefinition,
    inventory: &InventoryStore,
    options: &PlanOptions,
    replaced: &BTreeSet<String>,
) -> Result<Plan> {
    plan_listed(cloud, pool, target, inventory, options, replaced).map(|(plan, _)| plan)
}

/// What archive planning learned that the drive part reuses.
pub(super) struct Listed {
    pub listings: BTreeMap<String, RemoteListing>,
    /// New shards of the archive entries, for the combined quota check.
    pub specs: Vec<Vec<PhysicalSpec>>,
}

/// `plan_with_replaced`, also returning the listings and quota specs.
pub(super) fn plan_listed(
    cloud: &dyn Cloud,
    pool: &str,
    target: PoolDefinition,
    inventory: &InventoryStore,
    options: &PlanOptions,
    replaced: &BTreeSet<String>,
) -> Result<(Plan, Listed)> {
    for rate in [options.download_mib_s, options.upload_mib_s]
        .into_iter()
        .flatten()
    {
        if !rate.is_finite() || rate <= 0.0 {
            bail!("bandwidth must be a finite positive aggregate MiB/s rate");
        }
    }
    let workers = if options.workers > 0 {
        options.workers
    } else {
        target.workers.max(1)
    };
    let target_set: BTreeSet<String> = target.remotes.iter().cloned().collect();
    let configured = cloud.configured();
    let mut listings = list_all(cloud, &target_set, configured.as_ref());
    // Remotes of pool archives in the inventory that left the pool.
    let extra: BTreeSet<String> = inventory
        .entries
        .values()
        .filter(|e| super::enumerate::in_pool(e, &target_set))
        .flat_map(|e| e.remotes.iter().cloned())
        .filter(|r| !listings.contains_key(r))
        .collect();
    listings.extend(list_all(cloud, &extra, configured.as_ref()));
    let enumerated = enumerate(cloud, &target_set, inventory, &listings);
    // Shard remotes of manifests found only in the cloud.
    let extra: BTreeSet<String> = enumerated
        .found
        .iter()
        .filter_map(|f| match f {
            Found::Loaded(l) => Some(l),
            Found::Unreadable { .. } => None,
        })
        .flat_map(|l| l.manifest.shards.iter().map(|s| s.remote.clone()))
        .filter(|r| !listings.contains_key(r))
        .collect();
    listings.extend(list_all(cloud, &extra, configured.as_ref()));

    let mut notes = Vec::new();
    for (remote, listing) in &listings {
        let in_pool = target_set.contains(remote);
        match (listing, in_pool) {
            (RemoteListing::NotConfigured, true) => notes.push(format!(
                "pool remote {remote} is not configured on this PC; its shards are unknown"
            )),
            (RemoteListing::Failed(e), true) => notes.push(format!(
                "pool remote {remote} could not be listed ({e}); its shards are unknown"
            )),
            (RemoteListing::NotConfigured, false) => notes.push(format!(
                "removed remote {remote} is not configured; its shards count as unavailable (re-add it and plan again to copy them)"
            )),
            (RemoteListing::Failed(e), false) => notes.push(format!(
                "removed remote {remote} could not be listed ({e}); its shards count as unavailable"
            )),
            _ => {}
        }
    }

    let mut results: Vec<(Entry, Transfer)> = Vec::new();
    let mut skipped_replaced = 0usize;
    for found in &enumerated.found {
        let id = match found {
            Found::Loaded(l) => l.manifest.archive_id.as_str(),
            Found::Unreadable { archive_id, .. } => archive_id.as_str(),
        };
        if replaced.contains(id) {
            skipped_replaced += 1;
            continue;
        }
        match found {
            Found::Loaded(loaded) => results.push(plan_entry(
                cloud,
                loaded,
                &target,
                &target_set,
                &listings,
                options,
                workers,
            )?),
            Found::Unreadable {
                archive_id,
                original_name,
                size,
                source,
                detail,
            } => results.push((
                Entry {
                    archive_id: archive_id.clone(),
                    original_name: original_name.clone(),
                    size: *size,
                    source: source.clone(),
                    fingerprint: String::new(),
                    action: Action::Unknown,
                    download_bytes: 0,
                    upload_bytes: 0,
                    losses: Vec::new(),
                    detail: Some(detail.clone()),
                },
                Transfer::default(),
            )),
        }
    }
    results.sort_by(|a, b| a.0.archive_id.cmp(&b.0.archive_id));

    if skipped_replaced > 0 {
        notes.push(format!(
            "{skipped_replaced} original archive(s) already replaced by an earlier migration are skipped (their replacements are planned instead)"
        ));
    }
    let mut counts = Counts::default();
    let (mut download, mut upload, mut new_storage) = (0u64, 0u64, 0u64);
    let add = |a: u64, b: u64| a.checked_add(b).context("transfer estimate overflow");
    let (mut server_shards, mut server_bytes, mut server_readback) = (0usize, 0u64, 0usize);
    for (entry, transfer) in &results {
        match entry.action {
            Action::Unaffected => counts.unaffected += 1,
            Action::Relocate => counts.relocate += 1,
            Action::Reencode => counts.reencode += 1,
            Action::Lost => counts.lost += 1,
            Action::Unknown => counts.unknown += 1,
        }
        download = add(download, entry.download_bytes)?;
        upload = add(upload, entry.upload_bytes)?;
        new_storage = add(new_storage, transfer.specs.iter().map(|s| s.size).sum())?;
        if entry.action == Action::Relocate {
            server_shards += transfer.server_side_shards;
            server_bytes = add(server_bytes, transfer.server_side_bytes)?;
            server_readback += transfer.server_side_readback_shards;
        }
    }
    let specs: Vec<Vec<PhysicalSpec>> = results.iter().map(|(_, t)| t.specs.clone()).collect();
    let quota_ok = cloud.quota_ok(&target, &specs);

    if enumerated.drive_skipped > 0 {
        notes.push(if options.skip_drive {
            format!(
                "{} drive archive(s) (virtual-*) are not included: the drive was left out of this plan",
                enumerated.drive_skipped
            )
        } else {
            format!(
                "{} drive archive(s) (virtual-*) are not migrated as archives: the drive's visible files are planned in its own part and adopted into a new drive generation",
                enumerated.drive_skipped
            )
        });
    }
    notes.extend(enumerated.notes);
    if counts.unknown > 0 {
        notes.push(format!(
            "{} archive(s) could not be checked because of provider errors; they are never treated as lost. Plan again later",
            counts.unknown
        ));
    }
    if counts.lost > 0 {
        notes.push(format!(
            "{} archive(s) are unrecoverable: some group has fewer than K readable shards",
            counts.lost
        ));
    }
    if options.probe_full {
        notes.push("Full probe: shards of affected archives were read and hashed.".into());
    } else {
        notes.push("Quick probe: shard presence and sizes from one listing per remote; contents were not hashed.".into());
    }
    if target.native_crypt {
        notes.push("Native crypt writes the same rclone-crypt format, so existing archives are not re-encoded for it.".into());
    }
    if server_shards > 0 {
        let readback = if server_readback > 0 {
            format!(
                "; {server_readback} of them are read back because the provider reports no hash"
            )
        } else {
            "; they are verified by ciphertext hash".into()
        };
        notes.push(format!(
            "{server_shards} shard(s) ({}) are copied server-side by the provider (no transfer through this PC){readback}.",
            crate::presentation::format_bytes(server_bytes)
        ));
    }
    notes.push("Estimates count Unknown archives as if they will move. Relocation moves only shards on removed remotes (or over the outage bound): kept shards are copied on their remote (server-side when the provider can), readable moving shards are streamed, unreadable ones are rebuilt from K shards per group; every shard is verified by ciphertext hash or readback. Remotes whose copy features are unknown are estimated as streamed copies. Originals are kept, so new_storage_bytes is additional space.".into());

    let estimated_seconds = super::speed::estimate_seconds(
        download,
        upload,
        options.download_mib_s,
        options.upload_mib_s,
    );
    let plan = Plan {
        version: 1,
        migration_id: random_hex32()?,
        pool: pool.to_owned(),
        created_unix: crate::utils::now_unix(),
        created_by: pc_name(),
        target,
        entries: results.into_iter().map(|(e, _)| e).collect(),
        counts,
        download_bytes: download,
        upload_bytes: upload,
        new_storage_bytes: new_storage,
        estimated_seconds,
        download_mib_s: options.download_mib_s,
        upload_mib_s: options.upload_mib_s,
        quota_ok,
        notes,
    };
    Ok((plan, Listed { listings, specs }))
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
