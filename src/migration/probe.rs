//! Probe: decide per shard whether it is readable, and per Reed-Solomon group
//! whether the archive can still be rebuilt. A provider error is never taken
//! as a loss; only a remote that left the pool and cannot be read counts as
//! gone (`RemoteRemoved`).
use super::enumerate::RemoteListing;
use super::model::{GroupLoss, MissingReason, MissingShard};
use crate::manifest::{coding_group_count, data_shards};
use crate::prelude::*;
use crate::utils::relative_remote_object;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShardState {
    Ok,
    Missing(MissingReason),
    /// Could not be checked (provider error): may still be readable.
    Unknown(String),
}

impl ShardState {
    pub(crate) fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Quick probe from root listings: presence and size of every shard, in
/// `manifest.shards` order.
pub(crate) fn quick_states(
    manifest: &Manifest,
    listings: &BTreeMap<String, RemoteListing>,
    target: &BTreeSet<String>,
) -> Vec<ShardState> {
    manifest
        .shards
        .iter()
        .map(|shard| {
            let in_pool = target.contains(&shard.remote);
            match listings.get(&shard.remote) {
                Some(RemoteListing::Listed(files)) => {
                    match relative_remote_object(&shard.remote, &shard.object) {
                        Ok(path) => match files.get(&path) {
                            None => ShardState::Missing(MissingReason::Missing),
                            Some(size) if *size != shard.size => {
                                ShardState::Missing(MissingReason::BadSize)
                            }
                            Some(_) => ShardState::Ok,
                        },
                        Err(error) => ShardState::Unknown(format!("{error:#}")),
                    }
                }
                _ if !in_pool => ShardState::Missing(MissingReason::RemoteRemoved),
                Some(RemoteListing::NotConfigured) => {
                    ShardState::Unknown(format!("{} is not configured", shard.remote))
                }
                Some(RemoteListing::Failed(error)) => ShardState::Unknown(error.clone()),
                None => ShardState::Unknown(format!("{} was not listed", shard.remote)),
            }
        })
        .collect()
}

/// Full probe results (hash checked), in `manifest.shards` order.
pub(crate) fn full_states(
    manifest: &Manifest,
    probes: &[(Shard, Probe)],
    target: &BTreeSet<String>,
) -> Vec<ShardState> {
    let by_index: BTreeMap<u32, &Probe> = probes.iter().map(|(s, p)| (s.index, p)).collect();
    manifest
        .shards
        .iter()
        .map(|shard| match by_index.get(&shard.index) {
            Some(Probe::Ok) => ShardState::Ok,
            Some(Probe::Missing) => ShardState::Missing(MissingReason::Missing),
            Some(Probe::BadSize { .. }) => ShardState::Missing(MissingReason::BadSize),
            Some(Probe::Corrupt { .. }) => ShardState::Missing(MissingReason::Corrupt),
            Some(Probe::Error(_)) | None if !target.contains(&shard.remote) => {
                ShardState::Missing(MissingReason::RemoteRemoved)
            }
            Some(Probe::Error(error)) => ShardState::Unknown(error.clone()),
            None => ShardState::Unknown("shard was not probed".into()),
        })
        .collect()
}

/// Recoverability of one archive.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Availability {
    /// Groups with at least one shard that is not readable.
    pub losses: Vec<GroupLoss>,
    /// Some group cannot be rebuilt even if every unknown shard is readable.
    pub lost: bool,
    /// Not lost, but some group needs unknown shards to reach K.
    pub undetermined: bool,
}

/// Groups of `manifest` with their shard positions. Uncoded archives form one
/// group that needs every shard.
pub(crate) struct GroupView {
    pub group: u32,
    pub required_k: usize,
    /// Zero-filled data slots of a final partial group (always available).
    pub virtual_zero: usize,
    /// Positions into `manifest.shards`.
    pub members: Vec<usize>,
}

pub(crate) fn groups(manifest: &Manifest) -> Vec<GroupView> {
    let Some(coding) = &manifest.coding else {
        return vec![GroupView {
            group: 0,
            required_k: manifest.shards.len(),
            virtual_zero: 0,
            members: (0..manifest.shards.len()).collect(),
        }];
    };
    let count = coding_group_count(data_shards(manifest).len(), coding.data_shards);
    (0..count as u32)
        .map(|group| {
            let members: Vec<usize> = manifest
                .shards
                .iter()
                .enumerate()
                .filter(|(_, s)| s.group == group)
                .map(|(i, _)| i)
                .collect();
            let real_data = members
                .iter()
                .filter(|&&i| manifest.shards[i].kind == ShardKind::Data)
                .count();
            GroupView {
                group,
                required_k: coding.data_shards,
                virtual_zero: coding.data_shards.saturating_sub(real_data),
                members,
            }
        })
        .collect()
}

pub(crate) fn assess(manifest: &Manifest, states: &[ShardState]) -> Availability {
    let mut out = Availability::default();
    for view in groups(manifest) {
        let mut ok = 0usize;
        let mut unknown = 0usize;
        let mut missing = Vec::new();
        for &i in &view.members {
            let shard = &manifest.shards[i];
            let reason = match &states[i] {
                ShardState::Ok => {
                    ok += 1;
                    continue;
                }
                ShardState::Missing(reason) => *reason,
                ShardState::Unknown(_) => {
                    unknown += 1;
                    MissingReason::ProviderError
                }
            };
            missing.push(MissingShard {
                index: shard.index,
                remote: shard.remote.clone(),
                reason,
            });
        }
        let available = ok + view.virtual_zero;
        if available + unknown < view.required_k {
            out.lost = true;
        } else if available < view.required_k {
            out.undetermined = true;
        }
        if !missing.is_empty() {
            out.losses.push(GroupLoss {
                group: view.group,
                required_k: view.required_k,
                available,
                missing,
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
