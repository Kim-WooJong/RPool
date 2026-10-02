//! Shard placement: maps the physical shards of an upload or relocation to
//! target remotes under the pool's `Placement` policy and account quotas.

/// Placement policies and relocation slot assignment.
mod assign;
/// Quota-driven free-ratio, proportional and capacity-first allocation.
mod free_ratio;

pub(crate) use assign::validate_resilient_plan;
pub(crate) use assign::{
    assign_remotes, assign_replacement_slots, assign_with_budget, outage_domains, ReplacementSlot,
};
pub(crate) use free_ratio::plan_free_ratio;
