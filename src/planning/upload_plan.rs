use crate::manifest::coding_group_count;
use crate::placement::assign_remotes;
use crate::prelude::*;
use crate::utils::remote_join;

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
        (source_size / shard_size + u64::from(source_size % shard_size != 0)) as usize
    };

    let mut plan_shards = Vec::new();
    let mut specs = Vec::new();

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
                specs.push(PhysicalSpec {
                    group: group as u32,
                    size,
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
                specs.push(PhysicalSpec {
                    group: group as u32,
                    size: shard_size,
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
            specs.push(PhysicalSpec {
                group: data_index as u32,
                size,
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
