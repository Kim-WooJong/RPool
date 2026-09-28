use crate::prelude::*;
use crate::maintenance::analyze_groups;

pub(crate) fn group_recoverability(manifest: &Manifest, probes: &[(Shard, Probe)]) -> (usize, usize) {
    let groups = analyze_groups(manifest, probes);
    let degraded = groups.iter().filter(|group| group.status != "healthy").count();
    let unrecoverable = groups.iter().filter(|group| group.is_unrecoverable()).count();
    (degraded, unrecoverable)
}
