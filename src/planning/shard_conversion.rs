//! Turns a planned shard into the manifest's `Shard` once its hash is known.

use crate::prelude::*;

/// Manifest shard for `plan` with its uploaded BLAKE3 `blake3`. Used by
/// `put` and `storage::data_upload`.
pub(crate) fn shard_from_plan(plan: &PlanShard, blake3: String) -> Shard {
    Shard {
        index: plan.index,
        offset: plan.offset,
        size: plan.size,
        remote: plan.remote.clone(),
        object: plan.object.clone(),
        blake3,
        kind: plan.kind,
        group: plan.group,
        slot: plan.slot,
    }
}
