//! Shard placement policy ([`Placement`]): how `put`, the drive and
//! migrations spread the shards of a coding group over a pool's remotes.
//!
//! Users choose a method ([`PlacementMethod`]) and one "provider outage
//! protection" switch; [`Placement::from_parts`] combines them into the policy
//! the engine runs. Pool files keep writing the pre-2.21 value in
//! `placement` (see [`Placement::legacy`]) plus `outage_protection`, so older
//! RPool still opens pools that use one of the new combinations.
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
/// Saved in pool definitions (kebab-case) and chosen on the CLI
/// (`--placement`, plus `--outage-protection`) or in the GUI.
pub(crate) enum Placement {
    /// Cycle through the remotes in order.
    RoundRobin,
    /// Highest free ratio first, at most M shards per account per group.
    FreeRatio,
    /// Highest free ratio first, without a built-in per-account outage bound.
    FreeRatioUnbounded,
    /// Spread each coding group (and its data shards) evenly over the
    /// accounts for read speed; no per-account outage bound.
    #[value(alias = "spread")]
    Proportional,
    /// Require each resolved backing target to hold at most M shards per group.
    Resilient,
    /// Spend independent account quotas without a provider-outage shard bound.
    #[value(alias = "capacity")]
    CapacityFirst,
    /// Round robin over backing accounts (aliases of one account count
    /// together), at most max(M, even share) shards per account per group.
    #[value(hide = true)]
    RoundRobinProtected,
    /// Even spread for read speed, at most max(M, even share) shards per
    /// account per group.
    #[value(hide = true)]
    ProportionalProtected,
    /// Largest remaining quota first, at most max(M, even share) shards per
    /// account per group.
    #[value(hide = true)]
    CapacityFirstProtected,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// The placement method a user picks; the outage switch is separate.
pub(crate) enum PlacementMethod {
    /// Cycle through the accounts.
    RoundRobin,
    /// Fill by free-space ratio.
    FreeRatio,
    /// Spread every coding group evenly (read speed first, usage second).
    Spread,
    /// Largest remaining quota first.
    CapacityFirst,
}

impl PlacementMethod {
    /// Every method, in picker order.
    pub(crate) const ALL: [Self; 4] = [
        Self::RoundRobin,
        Self::FreeRatio,
        Self::Spread,
        Self::CapacityFirst,
    ];

    /// English GUI label (translated with `tr` by the caller).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "Round robin",
            Self::FreeRatio => "Free-space ratio",
            Self::Spread => "Even spread (read speed first)",
            Self::CapacityFirst => "Capacity first",
        }
    }
}

impl Placement {
    /// The method part of this policy.
    pub(crate) fn method(self) -> PlacementMethod {
        match self {
            Self::RoundRobin | Self::RoundRobinProtected => PlacementMethod::RoundRobin,
            Self::FreeRatio | Self::FreeRatioUnbounded | Self::Resilient => {
                PlacementMethod::FreeRatio
            }
            Self::Proportional | Self::ProportionalProtected => PlacementMethod::Spread,
            Self::CapacityFirst | Self::CapacityFirstProtected => PlacementMethod::CapacityFirst,
        }
    }

    /// Whether the policy bounds the shards one account (or outage group)
    /// holds of a coding group.
    pub(crate) fn outage_protection(self) -> bool {
        matches!(
            self,
            Self::FreeRatio
                | Self::Resilient
                | Self::RoundRobinProtected
                | Self::ProportionalProtected
                | Self::CapacityFirstProtected
        )
    }

    /// The policy for `method` with outage protection on or off. Free-space
    /// ratio with protection is per account; [`Self::Resilient`] (declared outage
    /// groups, strict) is chosen with `strict_outage_groups`.
    pub(crate) fn from_parts(
        method: PlacementMethod,
        protection: bool,
        strict_outage_groups: bool,
    ) -> Self {
        match (method, protection) {
            (PlacementMethod::RoundRobin, false) => Self::RoundRobin,
            (PlacementMethod::RoundRobin, true) => Self::RoundRobinProtected,
            (PlacementMethod::FreeRatio, false) => Self::FreeRatioUnbounded,
            (PlacementMethod::FreeRatio, true) if strict_outage_groups => Self::Resilient,
            (PlacementMethod::FreeRatio, true) => Self::FreeRatio,
            (PlacementMethod::Spread, false) => Self::Proportional,
            (PlacementMethod::Spread, true) => Self::ProportionalProtected,
            (PlacementMethod::CapacityFirst, false) => Self::CapacityFirst,
            (PlacementMethod::CapacityFirst, true) => Self::CapacityFirstProtected,
        }
    }

    /// This policy with outage protection switched on or off (same method;
    /// Resilient stays strict when kept on).
    pub(crate) fn with_protection(self, protection: bool) -> Self {
        Self::from_parts(self.method(), protection, self == Self::Resilient)
    }

    /// The nearest policy RPool before 2.21 knows; pool files store it in
    /// `placement` so older versions still open the pool.
    pub(crate) fn legacy(self) -> Self {
        match self {
            Self::RoundRobinProtected => Self::RoundRobin,
            Self::ProportionalProtected => Self::Proportional,
            Self::CapacityFirstProtected => Self::CapacityFirst,
            other => other,
        }
    }

    /// The new protected policies, which pool files spell as their
    /// [`Self::legacy`] value plus `outage_protection: true`.
    pub(crate) fn is_protected_variant(self) -> bool {
        self.legacy() != self
    }

    /// Per-account group bound of max(M, even share), like free-ratio.
    pub(crate) fn caps_like_free_ratio(self) -> bool {
        matches!(
            self,
            Self::FreeRatio
                | Self::RoundRobinProtected
                | Self::ProportionalProtected
                | Self::CapacityFirstProtected
        )
    }

    /// Policies that spread each group's data shards evenly (rebalance).
    pub(crate) fn spreads_data(self) -> bool {
        self.method() == PlacementMethod::Spread
    }

    /// Policies whose upload placement reads live account quotas.
    pub(crate) fn quota_aware(self) -> bool {
        !matches!(self, Self::RoundRobin | Self::RoundRobinProtected)
    }

    /// The CLI/JSON spelling (`round-robin`, ...), e.g. for generated commands.
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            Self::RoundRobin => "round-robin",
            Self::FreeRatio => "free-ratio",
            Self::FreeRatioUnbounded => "free-ratio-unbounded",
            Self::Proportional => "proportional",
            Self::Resilient => "resilient",
            Self::CapacityFirst => "capacity-first",
            Self::RoundRobinProtected => "round-robin-protected",
            Self::ProportionalProtected => "proportional-protected",
            Self::CapacityFirstProtected => "capacity-first-protected",
        }
    }

    /// English GUI label (translated with `tr` by the caller).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "Round robin",
            Self::FreeRatio => "Free-space ratio (provider-outage protection)",
            Self::FreeRatioUnbounded => "Free-space ratio",
            Self::Proportional => "Even spread (read speed first)",
            Self::Resilient => "Resilient (provider-outage bound)",
            Self::CapacityFirst => "Capacity first",
            Self::RoundRobinProtected => "Round robin (provider-outage protection)",
            Self::ProportionalProtected => "Even spread (provider-outage protection)",
            Self::CapacityFirstProtected => "Capacity first (provider-outage protection)",
        }
    }

    /// Warning/explanation shown under the placement picker in the GUI;
    /// `None` for round robin.
    pub(crate) fn protection_note(self) -> Option<&'static str> {
        match self {
            Self::Resilient => Some("Resilient: each declared outage group holds at most M shards per coding group. Independent outage groups are required."),
            Self::FreeRatio => Some("Free-space ratio: shards go to the accounts with the most free space by ratio, but one account holds at most M shards of each coding group, so losing one account stays recoverable when the pool has enough accounts."),
            Self::FreeRatioUnbounded => Some("Free-space ratio without outage guarantee: shards prefer accounts with the highest free-space percentage, without the M-shard per-account limit. Accounts with similar percentages still share a coding group; a configured account shard limit still applies. Losing one account may make a file unrecoverable."),
            Self::Proportional => Some("Even spread: each coding group and its data shards are spread evenly across every account with free space, so reads use every account at once; free-space ratios are balanced second. Larger accounts take the remainder once smaller ones fill; no per-account cap for durability. Reed-Solomon parity remains, but losing one account may make a file unrecoverable."),
            Self::CapacityFirst => Some("Capacity-first: uses large account quotas without an outage-group shard limit. Reed-Solomon parity remains, but losing one account or provider may make a file unrecoverable."),
            Self::RoundRobinProtected => Some("Round robin with provider-outage protection: shards cycle over the backing accounts (aliases of one account count together), and one account holds at most M shards of each coding group when the pool has enough accounts."),
            Self::ProportionalProtected => Some("Even spread with provider-outage protection: each coding group and its data shards are spread evenly for read speed, and no account takes more than M shards of a group even when smaller accounts are full, as long as the pool has enough accounts."),
            Self::CapacityFirstProtected => Some("Capacity first with provider-outage protection: large account quotas are used first, but one account holds at most M shards of each coding group when the pool has enough accounts."),
            Self::RoundRobin => None,
        }
    }
}

/// Warning when outage protection is on but `accounts` cannot hold a K+M
/// group with at most M shards each (`None` when it can, or protection is off).
pub(crate) fn outage_protection_gap(
    placement: Placement,
    data_shards: usize,
    parity_shards: usize,
    accounts: usize,
) -> Option<(usize, usize)> {
    if !placement.outage_protection() || accounts == 0 {
        return None;
    }
    let needed = if parity_shards == 0 {
        usize::MAX
    } else {
        (data_shards + parity_shards).div_ceil(parity_shards)
    };
    (accounts < needed).then_some((accounts, needed))
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
        assert_eq!(
            serde_json::to_string(&Placement::FreeRatioUnbounded).unwrap(),
            "\"free-ratio-unbounded\""
        );
        assert_eq!(
            <Placement as ValueEnum>::from_str("free-ratio-unbounded", true).unwrap(),
            Placement::FreeRatioUnbounded
        );
    }

    #[test]
    fn method_and_switch_round_trip_every_policy() {
        for placement in [
            Placement::RoundRobin,
            Placement::FreeRatio,
            Placement::FreeRatioUnbounded,
            Placement::Proportional,
            Placement::Resilient,
            Placement::CapacityFirst,
            Placement::RoundRobinProtected,
            Placement::ProportionalProtected,
            Placement::CapacityFirstProtected,
        ] {
            let back = Placement::from_parts(
                placement.method(),
                placement.outage_protection(),
                placement == Placement::Resilient,
            );
            assert_eq!(back, placement);
            assert_eq!(placement.legacy().method(), placement.method());
        }
        assert_eq!(
            Placement::Proportional.with_protection(true),
            Placement::ProportionalProtected
        );
        assert_eq!(
            Placement::FreeRatio.with_protection(false),
            Placement::FreeRatioUnbounded
        );
        assert_eq!(
            Placement::Resilient.with_protection(true),
            Placement::Resilient
        );
        assert_eq!(
            <Placement as ValueEnum>::from_str("spread", true).unwrap(),
            Placement::Proportional
        );
    }

    #[test]
    fn protection_gap_reports_too_few_accounts() {
        // 9+3 needs 4 accounts for at most 3 shards each.
        assert_eq!(
            outage_protection_gap(Placement::ProportionalProtected, 9, 3, 3),
            Some((3, 4))
        );
        assert_eq!(
            outage_protection_gap(Placement::ProportionalProtected, 9, 3, 4),
            None
        );
        assert_eq!(
            outage_protection_gap(Placement::Proportional, 9, 3, 3),
            None
        );
    }
}
