//! Shard placement policy ([`Placement`]): how `put`, the drive and
//! migrations spread the shards of a coding group over a pool's remotes.
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
/// Saved in pool definitions (kebab-case) and chosen on the CLI
/// (`--placement`) or in the GUI pool/settings screens.
pub(crate) enum Placement {
    /// Cycle through the remotes in order.
    RoundRobin,
    /// Highest free ratio first, at most M shards per account per group.
    FreeRatio,
    /// Highest free ratio first with no per-account shard limit.
    Proportional,
    /// Require each resolved backing target to hold at most M shards per group.
    Resilient,
    /// Spend independent account quotas without a provider-outage shard bound.
    CapacityFirst,
}

impl Placement {
    /// The CLI/JSON spelling (`round-robin`, ...), e.g. for generated commands.
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            Self::RoundRobin => "round-robin",
            Self::FreeRatio => "free-ratio",
            Self::Proportional => "proportional",
            Self::Resilient => "resilient",
            Self::CapacityFirst => "capacity-first",
        }
    }

    /// English GUI label (translated with `tr` by the caller).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "Round robin",
            Self::FreeRatio => "Free-space ratio",
            Self::Proportional => "Proportional fill (no provider-outage guarantee)",
            Self::Resilient => "Resilient (provider-outage bound)",
            Self::CapacityFirst => "Capacity-first (no provider-outage guarantee)",
        }
    }

    /// Warning/explanation shown under the placement picker in the GUI;
    /// `None` for round robin.
    pub(crate) fn protection_note(self) -> Option<&'static str> {
        match self {
            Self::Resilient => Some("Resilient: each declared outage group holds at most M shards per coding group. Independent outage groups are required."),
            Self::FreeRatio => Some("Free-space ratio: shards go to the accounts with the most free space by ratio, but one account holds at most M shards of each coding group, so losing one account stays recoverable when the pool has enough accounts."),
            Self::Proportional => Some("Proportional fill: shards go to the accounts with the most free space by ratio, with no per-account limit. Reed-Solomon parity remains, but losing one account may make a file unrecoverable."),
            Self::CapacityFirst => Some("Capacity-first: uses large account quotas without an outage-group shard limit. Reed-Solomon parity remains, but losing one account or provider may make a file unrecoverable."),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_outage_modes_keep_distinct_saved_names() {
        assert_eq!(
            serde_json::to_string(&Placement::Resilient).unwrap(),
            "\"resilient\""
        );
        assert_eq!(
            serde_json::from_str::<Placement>("\"resilient\"").unwrap(),
            Placement::Resilient
        );
        assert_eq!(
            serde_json::to_string(&Placement::CapacityFirst).unwrap(),
            "\"capacity-first\""
        );
        assert_eq!(
            serde_json::from_str::<Placement>("\"capacity-first\"").unwrap(),
            Placement::CapacityFirst
        );
    }
}
