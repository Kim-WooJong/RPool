//! Byte estimates per archive and the conservative quota check.
use super::enumerate::CopyFeatures;
use super::probe::{groups, ShardState};
use crate::prelude::*;
use crate::storage::admin::budget::BudgetSnapshot;

/// Planned transfer of one archive.
#[derive(Debug, Clone, Default)]
pub(crate) struct Transfer {
    /// Bytes to download.
    pub download: u64,
    /// Bytes to upload.
    pub upload: u64,
    /// Extra cloud space of the new shards (originals are kept).
    pub new_storage: u64,
    /// New shards (group, size) for the quota check.
    pub specs: Vec<PhysicalSpec>,
    /// Shards expected to be copied by the provider (server-side).
    pub server_side_shards: usize,
    /// Bytes of the server-side copied shards.
    pub server_side_bytes: u64,
    /// Of those, shards read back because the base reports no hash.
    pub server_side_readback_shards: usize,
}

/// Relocation, as `relocate` performs it. The replacement is a full copy
/// under a new archive id (it never borrows the old archive's objects);
/// `features` gives each remote's copy abilities (None = unknown).
/// Per shard (all counted once in `new_storage`):
/// - kept on its remote (readable, not moving) whose backend copies
///   server-side: no transfer; plus a readback download when the base reports
///   no ciphertext hash;
/// - otherwise, in a group without unreadable shards: a streamed copy
///   (download + upload) and a readback;
/// - in a group with an unreadable shard: `required` readable shards are
///   downloaded (those not copied server-side first), every non-server-side
///   shard is uploaded and read back.
///
/// Unknown features count as no server-side copy. Nothing moves when no shard
/// needs to (`moving` empty).
pub(crate) fn relocate_transfer(
    manifest: &Manifest,
    states: &[ShardState],
    moving: &BTreeSet<u32>,
    features: &dyn Fn(&str) -> Option<CopyFeatures>,
) -> Result<Transfer> {
    if moving.is_empty() {
        return Ok(Transfer::default());
    }
    copy_transfer(manifest, states, moving, features)
}

/// Per coding group: streamed copy plus readback when no shard is lost,
/// otherwise download of the readable shards needed for reconstruction and
/// upload/readback of every shard not copied server-side.
fn copy_transfer(
    manifest: &Manifest,
    states: &[ShardState],
    moving: &BTreeSet<u32>,
    features: &dyn Fn(&str) -> Option<CopyFeatures>,
) -> Result<Transfer> {
    let mut out = Transfer::default();
    let add = |a: u64, b: u64| a.checked_add(b).context("transfer estimate overflow");
    for view in groups(manifest) {
        // Provider copy abilities of each member (None: not copied server-side).
        let provider: Vec<Option<CopyFeatures>> = view
            .members
            .iter()
            .map(|&i| {
                let shard = &manifest.shards[i];
                let kept = states[i].is_ok() && !moving.contains(&shard.index);
                kept.then(|| features(shard.remote.as_str()))
                    .flatten()
                    .filter(|f| f.server_side_copy)
            })
            .collect();
        let rebuild = view.members.iter().any(|&i| !states[i].is_ok());
        if rebuild {
            let need = view.required_k.saturating_sub(view.virtual_zero);
            let mut readable: Vec<(bool, u64)> = view
                .members
                .iter()
                .zip(&provider)
                .filter(|(&i, _)| states[i].is_ok())
                .map(|(&i, p)| (p.is_some(), manifest.shards[i].size))
                .collect();
            // Same order as relocate; within a class the largest shards count.
            readable.sort_by_key(|&(server, size)| (server, std::cmp::Reverse(size)));
            for (_, size) in readable.into_iter().take(need) {
                out.download = add(out.download, size)?;
            }
        }
        for (&i, provider) in view.members.iter().zip(&provider) {
            let shard = &manifest.shards[i];
            out.specs.push(PhysicalSpec {
                group: shard.group,
                size: shard.size,
            });
            out.new_storage = add(out.new_storage, shard.size)?;
            if let Some(features) = provider {
                out.server_side_shards += 1;
                out.server_side_bytes = add(out.server_side_bytes, shard.size)?;
                if !features.ciphertext_hash {
                    out.server_side_readback_shards += 1;
                    out.download = add(out.download, shard.size)?;
                }
            } else if !rebuild {
                // Streamed copy, then readback.
                out.download = add(out.download, shard.size)?;
                out.download = add(out.download, shard.size)?;
                out.upload = add(out.upload, shard.size)?;
            } else {
                // Uploaded from the local copy or rebuild, then read back.
                out.upload = add(out.upload, shard.size)?;
                out.download = add(out.download, shard.size)?;
            }
        }
    }
    Ok(out)
}

/// Re-encoding: the reprocess formula (full restore, re-put, readback, verify).
pub(crate) fn reencode_transfer(manifest: &Manifest, target: &PoolDefinition) -> Result<Transfer> {
    let size = manifest.original_size;
    let new_storage = crate::pool::estimate::storage_bytes(size, target)?;
    let (download, upload) = crate::pool::estimate::reencode_transfer(size, new_storage)?;
    let coding = (target.parity_shards > 0).then(|| Coding {
        algorithm: RS_ALGORITHM.into(),
        data_shards: target.data_shards,
        parity_shards: target.parity_shards,
        stripe_size: 1048576,
    });
    let specs =
        crate::planning::physical_specs(size, target.shard_bytes()?.get(), coding.as_ref())?;
    Ok(Transfer {
        download,
        upload,
        new_storage,
        specs,
        ..Transfer::default()
    })
}

/// Places every archive's new shards in turn against the quota snapshot,
/// charging each capacity group. False as soon as one does not fit.
pub(crate) fn fits(
    snapshot: &BudgetSnapshot,
    placement: Placement,
    parity: Option<usize>,
    specs: &[Vec<PhysicalSpec>],
) -> bool {
    let mut snapshot = BudgetSnapshot {
        targets: snapshot.targets.clone(),
        rejected: vec![],
        observed_targets: vec![],
    };
    for archive in specs.iter().filter(|s| !s.is_empty()) {
        let Ok(assigned) =
            crate::placement::assign_with_budget(&snapshot, archive, placement, parity)
        else {
            return false;
        };
        for (spec, index) in archive.iter().zip(assigned) {
            let domain = snapshot.targets[index].capacity_domain.clone();
            for target in &mut snapshot.targets {
                if target.capacity_domain == domain {
                    target.free = target.free.saturating_sub(spec.size);
                }
            }
        }
    }
    true
}

/// Quota check against the pool's accounts (read-only `about` queries).
/// None when quotas are unknown, or when the new shards do not fit only
/// because some account could not be queried.
pub(crate) fn quota_ok(
    admin: &dyn crate::storage::admin::BackendAdmin,
    target: &PoolDefinition,
    specs: &[Vec<PhysicalSpec>],
) -> Option<bool> {
    if specs.iter().all(Vec::is_empty) {
        return Some(true);
    }
    let status = crate::mount::capacity::CapacityStatus::inspect(admin, target).ok()?;
    if status.targets.is_empty() {
        return None;
    }
    let snapshot = BudgetSnapshot {
        targets: status.targets.clone(),
        rejected: vec![],
        observed_targets: vec![],
    };
    let parity = (target.parity_shards > 0).then_some(target.parity_shards);
    if fits(&snapshot, target.placement, parity, specs) {
        Some(true)
    } else if status.excluded.is_empty() {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "estimate_tests.rs"]
mod tests;
