use crate::prelude::*;

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
