//! Configured remote names are not evidence of independent provider outages.
use crate::prelude::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureSafety {
    Safe,
    Unsafe,
    Unknown,
}
impl std::fmt::Display for FailureSafety {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Safe => "true",
            Self::Unsafe => "false",
            Self::Unknown => "unknown",
        })
    }
}
fn configured_concentration<'a>(
    shards: impl Iterator<Item = (u32, &'a str)>,
    parity: usize,
) -> (FailureSafety, usize) {
    let mut groups: BTreeMap<u32, BTreeMap<String, usize>> = BTreeMap::new();
    for (group, remote) in shards {
        // Paths within a configured remote never create independent domains.
        let name = remote
            .split_once(':')
            .map(|(name, _)| name)
            .unwrap_or(remote);
        *groups
            .entry(group)
            .or_default()
            .entry(name.to_owned())
            .or_default() += 1;
    }
    let max = groups
        .values()
        .flat_map(|g| g.values())
        .copied()
        .max()
        .unwrap_or(0);
    (
        if max > parity {
            FailureSafety::Unsafe
        } else {
            FailureSafety::Unknown
        },
        max,
    )
}
pub(crate) fn warn_plan_failure_domains(plan: &UploadPlan, coding: &Coding) {
    let (safety, max) = configured_concentration(
        plan.shards.iter().map(|s| (s.group, s.remote.as_str())),
        coding.parity_shards,
    );
    eprintln!("[warning] single-provider failure safety={safety}; max shards per configured remote={max}, parity={}. Distinct aliases/accounts are not proof of independent failure domains.",coding.parity_shards);
}
pub(crate) fn manifest_single_provider_failure_safety(
    manifest: &Manifest,
    coding: &Coding,
) -> (FailureSafety, usize) {
    configured_concentration(
        manifest.shards.iter().map(|s| (s.group, s.remote.as_str())),
        coding.parity_shards,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_names_do_not_prove_independence_and_paths_share_a_remote() {
        assert_eq!(
            configured_concentration([(0, "a:x"), (0, "b:x")].into_iter(), 1),
            (FailureSafety::Unknown, 1)
        );
        assert_eq!(
            configured_concentration([(0, "a:x"), (0, "a:y")].into_iter(), 1),
            (FailureSafety::Unsafe, 2)
        );
    }
}
