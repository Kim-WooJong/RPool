use crate::prelude::*;

pub(crate) fn data_shards(manifest: &Manifest) -> Vec<&Shard> {
    manifest
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Data)
        .collect()
}

pub(crate) fn coding_group_count(data_count: usize, data_shards_per_group: usize) -> usize {
    if data_count == 0 {
        0
    } else {
        (data_count + data_shards_per_group - 1) / data_shards_per_group
    }
}
