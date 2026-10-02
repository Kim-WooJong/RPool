//! Builds the `UploadPlan` of a file: data shards (and parity shards per
//! coding group), object names and target remotes.

use crate::manifest::coding_group_count;
use crate::placement::assign_remotes;
use crate::prelude::*;
use crate::utils::remote_join;

/// Plans the shards of a `source_size`-byte file as `archive_id`: data
/// shards (`data/` or `shards/`), whole-size parity per coding group, each
/// assigned a remote via `assign_remotes`. Used by `put` and mount uploads.
pub(crate) fn build_upload_plan(
    rclone: &str,
    source_size: u64,
    shard_size: u64,
    archive_id: String,
    remotes: Vec<String>,
    placement: Placement,
    coding: Option<Coding>,
) -> Result<UploadPlan> {
    let data_count = if source_size == 0 {
        1usize
    } else {
        (source_size / shard_size + u64::from(!source_size.is_multiple_of(shard_size))) as usize
    };

    let mut plan_shards = Vec::new();
    let specs = physical_specs(source_size, shard_size, coding.as_ref())?;

    if let Some(coding) = &coding {
        let groups = coding_group_count(data_count, coding.data_shards);
        for group in 0..groups {
            let group_start = group * coding.data_shards;
            let group_end = (group_start + coding.data_shards).min(data_count);
            for data_index in group_start..group_end {
                let offset = data_index as u64 * shard_size;
                let size = if source_size == 0 {
                    0
                } else {
                    (source_size - offset).min(shard_size)
                };
                let relative = format!("{archive_id}/data/{data_index:08}.bin");
                plan_shards.push(PlanShard {
                    index: data_index as u32,
                    offset,
                    size,
                    remote: String::new(),
                    object: relative,
                    kind: ShardKind::Data,
                    group: group as u32,
                    slot: (data_index - group_start) as u16,
                });
            }

            for parity_index in 0..coding.parity_shards {
                let physical_index = data_count + group * coding.parity_shards + parity_index;
                let relative = format!("{archive_id}/parity/g{group:08}-p{parity_index:03}.bin");
                plan_shards.push(PlanShard {
                    index: physical_index as u32,
                    offset: 0,
                    size: shard_size,
                    remote: String::new(),
                    object: relative,
                    kind: ShardKind::Parity,
                    group: group as u32,
                    slot: (coding.data_shards + parity_index) as u16,
                });
            }
        }
    } else {
        for data_index in 0..data_count {
            let offset = data_index as u64 * shard_size;
            let size = if source_size == 0 {
                0
            } else {
                (source_size - offset).min(shard_size)
            };
            let relative = format!("{archive_id}/shards/{data_index:08}.bin");
            plan_shards.push(PlanShard {
                index: data_index as u32,
                offset,
                size,
                remote: String::new(),
                object: relative,
                kind: ShardKind::Data,
                group: 0,
                slot: 0,
            });
        }
    }

    let remote_indexes = assign_remotes(
        rclone,
        &remotes,
        &specs,
        placement,
        coding.as_ref().map(|c| c.parity_shards),
    )?;

    for (shard, remote_index) in plan_shards.iter_mut().zip(remote_indexes) {
        let remote = remotes[remote_index].clone();
        shard.object = remote_join(&remote, &shard.object);
        shard.remote = remote;
    }

    plan_shards.sort_by_key(|s| s.index);

    Ok(UploadPlan {
        version: 2,
        archive_id,
        source_size,
        shard_size,
        remotes,
        placement,
        coding,
        shards: plan_shards,
    })
}

/// Exactly the upload planner's data/whole-parity ordering, also used for admission.
pub(crate) fn physical_specs(
    size: u64,
    shard: u64,
    coding: Option<&Coding>,
) -> Result<Vec<PhysicalSpec>> {
    if shard == 0 {
        bail!("zero shard size");
    }
    let data = size.div_ceil(shard).max(1);
    let k = coding.map_or(1, |c| c.data_shards as u64);
    let m = coding.map_or(0, |c| c.parity_shards as u64);
    if k == 0 {
        bail!("zero data shards");
    }
    let groups = data.div_ceil(k);
    let count = data
        .checked_add(groups.checked_mul(m).context("shard count overflow")?)
        .context("shard count overflow")?;
    if count > u32::MAX as u64 {
        bail!("too many shards");
    }
    let mut specs = Vec::new();
    specs.try_reserve_exact(usize::try_from(count)?)?;
    for group in 0..groups {
        for index in (group * k)..((group + 1) * k).min(data) {
            specs.push(PhysicalSpec {
                group: group as u32,
                size: size.saturating_sub(index * shard).min(shard),
            });
        }
        for _ in 0..m {
            specs.push(PhysicalSpec {
                group: group as u32,
                size: shard,
            });
        }
    }
    Ok(specs)
}
