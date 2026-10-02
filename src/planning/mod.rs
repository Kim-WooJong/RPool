//! Upload planning: the physical shard layout of a file, its remote
//! assignment (`placement`) and outage-safety checks of the result.

/// Single-provider outage safety of plans and manifests.
mod failure_domains;
/// Conversion of a planned shard into a manifest shard.
mod shard_conversion;
/// Upload plan construction (shard layout and remote assignment).
mod upload_plan;

pub(crate) use failure_domains::{
    manifest_single_provider_failure_safety, warn_plan_failure_domains, FailureSafety,
};
pub(crate) use shard_conversion::shard_from_plan;
pub(crate) use upload_plan::{build_upload_plan, physical_specs};
