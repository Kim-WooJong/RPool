//! Small queries over a manifest's shard list.

use crate::prelude::*;

/// The data shards in manifest order (parity excluded).
pub(crate) fn data_shards(manifest: &Manifest) -> Vec<&Shard> {
    manifest
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Data)
        .collect()
}

/// Number of coding groups for `data_count` data shards, `data_shards_per_group`
/// per group (last group may be short); 0 for no data.
pub(crate) fn coding_group_count(data_count: usize, data_shards_per_group: usize) -> usize {
    if data_count == 0 {
        0
    } else {
        data_count.div_ceil(data_shards_per_group)
    }
}
