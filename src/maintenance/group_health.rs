//! Per-coding-group health from shard probes.

use crate::manifest::{coding_group_count, data_shards};
use crate::prelude::*;

/// Classifies each coding group as `healthy`, `recoverable`, `unrecoverable`
/// or `provider-error` (a probe failed, so the state is unknown). A group is
/// recoverable while healthy shards plus virtual zero data shards (short last
/// group) still reach `data_shards`. Archives without coding form one group.
pub(crate) fn analyze_groups(manifest: &Manifest, probes: &[(Shard, Probe)]) -> Vec<GroupHealth> {
    let Some(coding) = &manifest.coding else {
        let bad_shards = probes
            .iter()
            .filter(|(_, probe)| {
                matches!(
                    probe,
                    Probe::Missing | Probe::BadSize { .. } | Probe::Corrupt { .. }
                )
            })
            .count();
        let provider_errors = probes
            .iter()
            .filter(|(_, probe)| matches!(probe, Probe::Error(_)))
            .count();
        let status = if provider_errors > 0 {
            "provider-error"
        } else if bad_shards > 0 {
            "unrecoverable"
        } else {
            "healthy"
        };
        return vec![GroupHealth {
            group: 0,
            status: status.to_string(),
            bad_shards,
            provider_errors,
        }];
    };

    let data = data_shards(manifest);
    let groups = coding_group_count(data.len(), coding.data_shards);
    let mut out = Vec::with_capacity(groups);

    for group in 0..groups {
        let group_id = group as u32;
        let real_data = data.iter().filter(|shard| shard.group == group_id).count();
        let virtual_zero = coding.data_shards.saturating_sub(real_data);
        let group_probes: Vec<&(Shard, Probe)> = probes
            .iter()
            .filter(|(shard, _)| shard.group == group_id)
            .collect();

        let bad_shards = group_probes
            .iter()
            .filter(|(_, probe)| {
                matches!(
                    probe,
                    Probe::Missing | Probe::BadSize { .. } | Probe::Corrupt { .. }
                )
            })
            .count();
        let provider_errors = group_probes
            .iter()
            .filter(|(_, probe)| matches!(probe, Probe::Error(_)))
            .count();
        let healthy_physical = group_probes
            .iter()
            .filter(|(_, probe)| probe.is_ok())
            .count();

        let status = if provider_errors > 0 {
            "provider-error"
        } else if bad_shards == 0 {
            "healthy"
        } else if healthy_physical + virtual_zero >= coding.data_shards {
            "recoverable"
        } else {
            "unrecoverable"
        };

        out.push(GroupHealth {
            group: group_id,
            status: status.to_string(),
            bad_shards,
            provider_errors,
        });
    }

    out
}
