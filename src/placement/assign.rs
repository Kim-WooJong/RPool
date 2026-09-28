use crate::prelude::*;
use crate::placement::plan_free_ratio;

pub(crate) fn assign_remotes(
    rclone: &str,
    remotes: &[String],
    specs: &[PhysicalSpec],
    placement: Placement,
    prefer_group_diversity: bool,
) -> Result<Vec<usize>> {
    match placement {
        Placement::RoundRobin => Ok((0..specs.len()).map(|i| i % remotes.len()).collect()),
        Placement::FreeRatio => plan_free_ratio(rclone, remotes, specs, prefer_group_diversity),
    }
}
