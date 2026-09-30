//! Byte estimates per archive and the conservative quota check.
use super::probe::{groups, ShardState};
use crate::prelude::*;
use crate::storage::admin::budget::BudgetSnapshot;

/// Planned transfer of one archive.
#[derive(Debug, Clone, Default)]
pub(crate) struct Transfer {
    pub download: u64,
    pub upload: u64,
    /// Extra cloud space of the new shards (originals are kept).
    pub new_storage: u64,
    /// New shards (group, size) for the quota check.
    pub specs: Vec<PhysicalSpec>,
}

/// Relocation, as `relocate` performs it: the replacement is a full copy
/// under a new archive id (it never borrows the old archive's objects), so
/// every readable shard is downloaded once and every shard (copied or
/// rebuilt from K readable ones) is uploaded. Each written shard is then read
/// back twice: the write readback and the final full scan. Nothing moves when
/// no shard needs to (`moving` empty).
pub(crate) fn relocate_transfer(
    manifest: &Manifest,
    states: &[ShardState],
    moving: &BTreeSet<u32>,
) -> Result<Transfer> {
    let mut out = Transfer::default();
    if moving.is_empty() {
        return Ok(out);
    }
    let add = |a: u64, b: u64| a.checked_add(b).context("transfer estimate overflow");
    for view in groups(manifest) {
        for &i in &view.members {
            let shard = &manifest.shards[i];
            if states[i].is_ok() {
                out.download = add(out.download, shard.size)?;
            }
            out.upload = add(out.upload, shard.size)?;
            out.specs.push(PhysicalSpec {
                group: shard.group,
                size: shard.size,
            });
        }
    }
    out.new_storage = out.upload;
    let readback = out
        .upload
        .checked_mul(2)
        .context("transfer estimate overflow")?;
    out.download = add(out.download, readback)?;
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
