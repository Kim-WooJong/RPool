//! How much of each account's free space a capped placement can ever use.
//!
//! Resilient puts at most M shards of a K+M coding group into one outage
//! group, free-ratio at most `max(M, ceil((K+M)/accounts))` into one account.
//! So one group never holds more than the share `c / (K+M)` of all shards,
//! and a group much larger than the others keeps space no placement can use.
//! With free space `v_i` per group, the shard bytes that fit are the largest
//! `S` with `S <= sum(min(v_i, c*S/(K+M)))` (a fractional bound: real
//! placement fills whole shards and may stop slightly earlier). Used by
//! `CapacityStatus` for the "unusable space" lines of `pool capacity` and the
//! GUI capacity summary.
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
/// One group's free space and the part of it the placement cannot use.
pub(crate) struct GroupBalance {
    /// Outage group (Resilient) or account (free-ratio).
    pub group: String,
    /// Free bytes of the group.
    pub free: u64,
    /// Free bytes no placement can fill with the current groups.
    pub unusable: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
/// Result of [`balance`].
pub(crate) struct CapacityBalance {
    /// Most shards each group may hold of one coding group.
    pub per_group_cap: usize,
    /// Shard bytes that fit in the current free space (upper bound).
    pub usable_physical: u64,
    /// The same as data after parity: `usable_physical * K / (K+M)`.
    pub usable_logical: u64,
    /// Every group, largest free space first.
    pub groups: Vec<GroupBalance>,
    /// Free space to add (in new groups no larger than the largest one) so
    /// that every group can be filled completely; 0 when already balanced.
    pub add_to_use_all: u64,
    /// Fewest new groups `add_to_use_all` needs (each at most the largest).
    pub add_groups_min: usize,
}

/// Balance of `free` (group name, free bytes) for coding groups of `width`
/// shards (`K+M`) with `data` data shards, when one group holds at most `cap`
/// shards of each coding group.
pub(crate) fn balance(
    free: &[(String, u64)],
    width: usize,
    data: usize,
    cap: usize,
) -> CapacityBalance {
    let mut groups: Vec<(String, u64)> = free.to_vec();
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (n, c) = (width.max(1) as u128, cap.min(width).max(1) as u128);
    // Fits when n*S <= sum(min(n*v_i, c*S)); the feasible S form an interval
    // from 0, so a binary search finds its end.
    let fits = |s: u128| {
        let held: u128 = groups
            .iter()
            .map(|(_, v)| (n * *v as u128).min(c * s))
            .sum();
        n * s <= held
    };
    let (mut low, mut high) = (0u128, groups.iter().map(|(_, v)| *v as u128).sum::<u128>());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if fits(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let usable = low;
    let share = |v: u64| (v as u128).min(c * usable / n) as u64;
    let total: u128 = groups.iter().map(|(_, v)| *v as u128).sum();
    let largest = groups.first().map_or(0, |(_, v)| *v);
    // Every group fills once the largest is at most the share c/n of all.
    let needed = (n * largest as u128).div_ceil(c);
    let add = needed.saturating_sub(total).min(u64::MAX as u128) as u64;
    CapacityBalance {
        per_group_cap: c as usize,
        usable_physical: usable.min(u64::MAX as u128) as u64,
        usable_logical: (usable * data.min(width) as u128 / n).min(u64::MAX as u128) as u64,
        groups: groups
            .iter()
            .map(|(group, v)| GroupBalance {
                group: group.clone(),
                free: *v,
                unusable: v - share(*v),
            })
            .collect(),
        add_to_use_all: add,
        add_groups_min: if add == 0 || largest == 0 {
            0
        } else {
            add.div_ceil(largest) as usize
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_000_000_000_000;
    const G: u64 = 1_000_000_000;

    fn disks(sizes: &[u64]) -> Vec<(String, u64)> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("d{i}"), *s))
            .collect()
    }

    /// 9+3 over 8T/4T/2T/150G: every group needs 3 shards on each of the four
    /// disks, so the 150 GB disk limits the pool to 4 x 150 GB of shards.
    #[test]
    fn nine_plus_three_over_uneven_disks_is_limited_by_the_smallest() {
        let b = balance(&disks(&[8 * T, 4 * T, 2 * T, 150 * G]), 12, 9, 3);
        assert_eq!(b.usable_physical, 600 * G);
        assert_eq!(b.usable_logical, 450 * G);
        assert_eq!(b.groups[0].unusable, 8 * T - 150 * G);
        assert_eq!(b.groups[3].unusable, 0);
        // The largest must be at most a quarter: 32 TB in all.
        assert_eq!(b.add_to_use_all, 32 * T - (14 * T + 150 * G));
        assert_eq!(b.add_groups_min, 3);
    }

    /// 6+4 over the same disks reaches the bound total - largest.
    #[test]
    fn six_plus_four_uses_everything_but_half_the_largest() {
        let b = balance(&disks(&[8 * T, 4 * T, 2 * T, 150 * G]), 10, 6, 4);
        assert_eq!(b.usable_physical, 10_250 * G);
        assert_eq!(b.usable_logical, 6_150 * G);
        assert_eq!(b.groups[0].unusable, 8 * T - 4_100 * G);
        assert!(b.groups[1..].iter().all(|g| g.unusable == 0));
    }

    /// Balanced groups waste nothing and need nothing added.
    #[test]
    fn balanced_groups_are_fully_usable() {
        let b = balance(
            &disks(&[8 * T, 8 * T, 8 * T, 4 * T, 2 * T, 2 * T]),
            12,
            9,
            3,
        );
        assert_eq!(b.usable_physical, 32 * T);
        assert_eq!(b.usable_logical, 24 * T);
        assert!(b.groups.iter().all(|g| g.unusable == 0));
        assert_eq!((b.add_to_use_all, b.add_groups_min), (0, 0));
    }

    /// Too few groups for a coding group: nothing fits.
    #[test]
    fn too_few_groups_fit_nothing() {
        let b = balance(&disks(&[8 * T, 4 * T, 2 * T]), 12, 9, 3);
        assert_eq!(b.usable_physical, 0);
        assert_eq!(b.groups[2].unusable, 2 * T);
        assert!(balance(&[], 12, 9, 3).groups.is_empty());
    }
}
