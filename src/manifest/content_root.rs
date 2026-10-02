//! Content-root checksums that seal a manifest's shard list.

use crate::prelude::*;

/// Version-1 root: BLAKE3 over index, offset, size and hash of every shard.
pub(crate) fn content_root_v1(shards: &[Shard]) -> String {
    let mut hasher = Hasher::new();
    for shard in shards {
        hasher.update(&shard.index.to_le_bytes());
        hasher.update(&shard.offset.to_le_bytes());
        hasher.update(&shard.size.to_le_bytes());
        hasher.update(shard.blake3.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Version-2 root: BLAKE3 over sizes, coding parameters and every shard's
/// index, kind, group, slot, offset, size, remote, object and hash.
pub(crate) fn content_root_v2(
    original_size: u64,
    shard_size: u64,
    coding: &Option<Coding>,
    shards: &[Shard],
) -> String {
    let mut hasher = Hasher::new();
    hasher.update(b"rpool-manifest-v2\0");
    hasher.update(&original_size.to_le_bytes());
    hasher.update(&shard_size.to_le_bytes());
    match coding {
        Some(coding) => {
            hasher.update(&[1u8]);
            hasher.update(&(coding.algorithm.len() as u64).to_le_bytes());
            hasher.update(coding.algorithm.as_bytes());
            hasher.update(&(coding.data_shards as u64).to_le_bytes());
            hasher.update(&(coding.parity_shards as u64).to_le_bytes());
            hasher.update(&(coding.stripe_size as u64).to_le_bytes());
        }
        None => {
            hasher.update(&[0u8]);
        }
    }
    for shard in shards {
        hasher.update(&shard.index.to_le_bytes());
        hasher.update(&[match shard.kind {
            ShardKind::Data => 0u8,
            ShardKind::Parity => 1u8,
        }]);
        hasher.update(&shard.group.to_le_bytes());
        hasher.update(&shard.slot.to_le_bytes());
        hasher.update(&shard.offset.to_le_bytes());
        hasher.update(&shard.size.to_le_bytes());
        hasher.update(&(shard.remote.len() as u64).to_le_bytes());
        hasher.update(shard.remote.as_bytes());
        hasher.update(&(shard.object.len() as u64).to_le_bytes());
        hasher.update(shard.object.as_bytes());
        hasher.update(shard.blake3.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}
