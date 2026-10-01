use crate::placement::plan_free_ratio;
use crate::prelude::*;

pub(crate) fn assign_remotes(
    rclone: &str,
    remotes: &[String],
    specs: &[PhysicalSpec],
    placement: Placement,
    parity_shards: Option<usize>,
) -> Result<Vec<usize>> {
    match placement {
        Placement::RoundRobin => balanced_assign(
            &remotes
                .iter()
                .map(|r| crate::storage::scheduler::remote_key(r))
                .collect::<Vec<_>>(),
            specs,
        ),
        Placement::FreeRatio => plan_free_ratio(rclone, remotes, specs, parity_shards),
        Placement::Proportional => plan_free_ratio(rclone, remotes, specs, None),
        Placement::Resilient | Placement::CapacityFirst => {
            use crate::storage::admin::{BackendAdmin, RcloneAdmin};
            let admin = RcloneAdmin::inherited(rclone);
            let catalog = admin.catalog()?;
            let snapshot =
                crate::storage::admin::budget::BudgetSnapshot::query(&admin, &catalog, remotes);
            if !snapshot.rejected.is_empty() {
                bail!("Quota planning unavailable: {:?}", snapshot.rejected);
            }
            let assigned = assign_with_budget(&snapshot, specs, placement, parity_shards)?;
            // Budget query may deduplicate/reorder targets. Never interpret its
            // indexes against the caller's original remote list.
            assigned
                .into_iter()
                .map(|i| {
                    remotes
                        .iter()
                        .position(|r| r == &snapshot.targets[i].remote)
                        .context("quota target missing from upload remotes")
                })
                .collect()
        }
    }
}

fn declared_failure(
    catalog: &crate::storage::admin::RemoteCatalog,
    remote: &str,
) -> Result<String> {
    catalog
        .capacity(remote)?
        .failure_domain
        .map(|d| d.as_str().to_owned())
        .context("Resilient placement requires a declared outage group for every backing remote")
}

pub(crate) fn assign_with_budget(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    placement: Placement,
    parity: Option<usize>,
) -> Result<Vec<usize>> {
    let assignments = match placement {
        Placement::FreeRatio => super::free_ratio::allocate(snapshot, specs, parity)?,
        Placement::Proportional => super::free_ratio::allocate(snapshot, specs, None)?,
        Placement::CapacityFirst => super::free_ratio::allocate_capacity_first(snapshot, specs)?,
        Placement::RoundRobin => balanced_assign(
            &snapshot
                .targets
                .iter()
                .map(|t| crate::storage::scheduler::remote_key(&t.remote))
                .collect::<Vec<_>>(),
            specs,
        )?,
        Placement::Resilient => resilient_allocate(snapshot, specs, parity.unwrap_or(0))?,
    };
    let mut budgets = snapshot.budgets();
    for (spec, index) in specs.iter().zip(&assignments) {
        let free = budgets
            .get_mut(&snapshot.targets[*index].capacity_domain)
            .context("missing quota group")?;
        *free = free
            .checked_sub(spec.size)
            .context("Selected placement exceeds an account quota; local data retained")?;
    }
    Ok(assignments)
}

/// Quota-aware two-choice placement, shared by admission and upload.
/// Largest shards first within each group avoid stranding full-size parity behind
/// a small final data shard. A stable candidate stream keeps capacity admission
/// and the actual plan consistent. Fall back to the full-candidate greedy pass when
/// a sampled path strands later shards; neither path relaxes quota/outage bounds.
fn resilient_allocate(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    parity: usize,
) -> Result<Vec<usize>> {
    resilient_allocate_inner(snapshot, specs, parity, ResilientChoice::Sampled)
        .or_else(|_| resilient_allocate_inner(snapshot, specs, parity, ResilientChoice::Weighted))
        .or_else(|_| resilient_allocate_inner(snapshot, specs, parity, ResilientChoice::Balanced))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResilientChoice {
    Sampled,
    Weighted,
    Balanced,
}

#[allow(clippy::too_many_arguments)] // sort comparator over borrowed placement state; a context struct would only add indirection
fn projected_utilization_cmp(
    a: usize,
    b: usize,
    spec: &PhysicalSpec,
    targets: &[crate::storage::admin::budget::TargetBudget],
    budgets: &BTreeMap<String, u64>,
    totals: &BTreeMap<String, u64>,
    domains: &[String],
    counts: &BTreeMap<(u32, &str), usize>,
) -> std::cmp::Ordering {
    let ta = &targets[a];
    let tb = &targets[b];
    let free_a = budgets[&ta.capacity_domain];
    let free_b = budgets[&tb.capacity_domain];
    let total_a = totals[&ta.capacity_domain].max(1);
    let total_b = totals[&tb.capacity_domain].max(1);
    let used_a = total_a.saturating_sub(free_a).saturating_add(spec.size);
    let used_b = total_b.saturating_sub(free_b).saturating_add(spec.size);
    (used_a as u128 * total_b as u128)
        .cmp(&(used_b as u128 * total_a as u128))
        .then_with(|| {
            counts
                .get(&(spec.group, domains[a].as_str()))
                .copied()
                .unwrap_or(0)
                .cmp(
                    &counts
                        .get(&(spec.group, domains[b].as_str()))
                        .copied()
                        .unwrap_or(0),
                )
        })
        .then_with(|| free_b.cmp(&free_a))
        .then_with(|| a.cmp(&b))
}

fn next_choice(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn choice_seed(snapshot: &crate::storage::admin::budget::BudgetSnapshot) -> u64 {
    // Stable across processes and file sizes: capacity binary search must see
    // the same candidate stream for the common prefix of shard groups.
    let mut seed = 0xcbf29ce484222325u64;
    for target in &snapshot.targets {
        for byte in target
            .remote
            .as_bytes()
            .iter()
            .chain(target.capacity_domain.as_bytes())
        {
            seed = (seed ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        for byte in target
            .free
            .to_le_bytes()
            .iter()
            .chain(target.total.to_le_bytes().iter())
        {
            seed = (seed ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
    }
    seed
}

fn resilient_allocate_inner(
    snapshot: &crate::storage::admin::budget::BudgetSnapshot,
    specs: &[PhysicalSpec],
    parity: usize,
    choice: ResilientChoice,
) -> Result<Vec<usize>> {
    let targets = &snapshot.targets;
    let domains = targets
        .iter()
        .map(|t| {
            t.failure_domain
                .clone()
                .context("Declare outage groups before Resilient placement")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut budgets = snapshot.budgets();
    let mut totals = BTreeMap::<String, u64>::new();
    for target in targets {
        totals
            .entry(target.capacity_domain.clone())
            .and_modify(|total| *total = (*total).min(target.total))
            .or_insert(target.total);
    }
    let mut random = choice_seed(snapshot);
    let mut counts = BTreeMap::<(u32, &str), usize>::new();
    let mut order: Vec<_> = (0..specs.len()).collect();
    order.sort_by_key(|&i| (specs[i].group, std::cmp::Reverse(specs[i].size), i));
    let mut result = vec![0; specs.len()];
    let mut cursor = 0;
    for i in order {
        let spec = &specs[i];
        let eligible: Vec<_> = (0..targets.len())
            .map(|n| (cursor + n) % targets.len())
            .filter(|&j| {
                budgets[&targets[j].capacity_domain] >= spec.size
                    && (spec.size == 0 && parity == 0
                        || counts
                            .get(&(spec.group, domains[j].as_str()))
                            .copied()
                            .unwrap_or(0)
                            < parity)
            })
            .collect();
        let chosen = if choice == ResilientChoice::Sampled {
            // Several rclone targets can draw on one account. Sample independent
            // quota domains, not aliases that would bias the lottery by count.
            let mut representatives = BTreeMap::<&str, usize>::new();
            for j in eligible {
                let domain = targets[j].capacity_domain.as_str();
                let count = counts
                    .get(&(spec.group, domains[j].as_str()))
                    .copied()
                    .unwrap_or(0);
                representatives
                    .entry(domain)
                    .and_modify(|previous| {
                        let old = counts
                            .get(&(spec.group, domains[*previous].as_str()))
                            .copied()
                            .unwrap_or(0);
                        if count < old {
                            *previous = j;
                        }
                    })
                    .or_insert(j);
            }
            let mut candidates: Vec<_> = representatives.into_values().collect();
            if candidates.is_empty() {
                bail!(
                    "Resilient placement cannot fit quota and outage bounds; local data retained"
                );
            }
            let first = (next_choice(&mut random) % candidates.len() as u64) as usize;
            let first = candidates.swap_remove(first);
            let second = if candidates.is_empty() {
                first
            } else {
                let index = (next_choice(&mut random) % candidates.len() as u64) as usize;
                candidates[index]
            };
            [first, second].into_iter().min_by(|&a, &b| {
                projected_utilization_cmp(a, b, spec, targets, &budgets, &totals, &domains, &counts)
            })
        } else if choice == ResilientChoice::Weighted {
            eligible.into_iter().min_by(|&a, &b| {
                projected_utilization_cmp(a, b, spec, targets, &budgets, &totals, &domains, &counts)
            })
        } else {
            eligible.into_iter().min_by_key(|&j| {
                (
                    counts
                        .get(&(spec.group, domains[j].as_str()))
                        .copied()
                        .unwrap_or(0),
                    std::cmp::Reverse(budgets[&targets[j].capacity_domain]),
                )
            })
        }
        .context("Resilient placement cannot fit quota and outage bounds; local data retained")?;
        *budgets.get_mut(&targets[chosen].capacity_domain).unwrap() -= spec.size;
        *counts
            .entry((spec.group, domains[chosen].as_str()))
            .or_default() += 1;
        result[i] = chosen;
        cursor = (chosen + 1) % targets.len();
    }
    ensure_parity_bound(&domains, specs, &result, parity)?;
    Ok(result)
}

pub(crate) fn balanced_assign(targets: &[String], specs: &[PhysicalSpec]) -> Result<Vec<usize>> {
    if targets.is_empty() {
        bail!("placement requires a target");
    }
    let mut counts = BTreeMap::<u32, BTreeMap<&str, usize>>::new();
    let mut result = Vec::with_capacity(specs.len());
    let mut cursor = 0;
    for spec in specs {
        let group = counts.entry(spec.group).or_default();
        let index = (0..targets.len())
            .map(|n| (cursor + n) % targets.len())
            .min_by_key(|i| group.get(targets[*i].as_str()).copied().unwrap_or(0))
            .unwrap();
        *group.entry(&targets[index]).or_default() += 1;
        result.push(index);
        cursor = (index + 1) % targets.len();
    }
    Ok(result)
}

pub(crate) fn validate_resilient_plan(rclone: &str, plan: &UploadPlan) -> Result<()> {
    use crate::storage::admin::{BackendAdmin, RcloneAdmin};
    if plan.placement != Placement::Resilient {
        return Ok(());
    }
    let catalog = RcloneAdmin::inherited(rclone).catalog()?;
    validate_resilient_catalog(&catalog, plan)
}

fn validate_resilient_catalog(
    catalog: &crate::storage::admin::RemoteCatalog,
    plan: &UploadPlan,
) -> Result<()> {
    let targets = plan
        .shards
        .iter()
        .map(|s| declared_failure(catalog, &s.object))
        .collect::<Result<Vec<_>>>()?;
    let specs = plan
        .shards
        .iter()
        .map(|s| PhysicalSpec {
            group: s.group,
            size: s.size,
        })
        .collect::<Vec<_>>();
    ensure_parity_bound(
        &targets,
        &specs,
        &(0..targets.len()).collect::<Vec<_>>(),
        plan.coding.as_ref().map_or(0, |c| c.parity_shards),
    )
}

/// One physical shard of an archive being relocated: its coding group, its
/// size, and the target index it already sits on (`None` when it must move).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplacementSlot {
    pub(crate) group: u32,
    pub(crate) size: u64,
    pub(crate) current: Option<usize>,
}

/// Outage group of each target remote, used by [`assign_replacement_slots`].
/// Resilient placement requires a declared outage group per backing remote
/// (as `put` does); other placements treat each configured remote name as its
/// own group, which is what `manifest_single_provider_failure_safety` checks.
pub(crate) fn outage_domains(
    rclone: &str,
    remotes: &[String],
    placement: Placement,
) -> Result<Vec<String>> {
    if placement == Placement::Resilient {
        use crate::storage::admin::{BackendAdmin, RcloneAdmin};
        let catalog = RcloneAdmin::inherited(rclone).catalog()?;
        return remotes
            .iter()
            .map(|remote| declared_failure(&catalog, remote))
            .collect();
    }
    Ok(remotes
        .iter()
        .map(|remote| {
            remote
                .split_once(':')
                .map_or(remote.as_str(), |(name, _)| name)
                .to_owned()
        })
        .collect())
}

/// Chooses a target for every slot of a relocated archive while moving as
/// little as possible. A slot keeps its `current` target unless that would
/// put more than `bound` shards of its group on one outage group (the first
/// kept shards in slot order stay, the excess moves). Moving slots go, largest
/// first, to the target whose outage group holds the fewest shards of that
/// coding group, then to the least loaded target. With `strict` (Resilient
/// placement) exceeding `bound` is an error; otherwise it is only avoided when
/// possible. `domains[i]` is the outage group of target `i`.
pub(crate) fn assign_replacement_slots(
    domains: &[String],
    slots: &[ReplacementSlot],
    bound: usize,
    strict: bool,
) -> Result<Vec<usize>> {
    if domains.is_empty() {
        bail!("relocation requires at least one target remote");
    }
    if let Some(slot) = slots
        .iter()
        .find(|slot| slot.current.is_some_and(|i| i >= domains.len()))
    {
        bail!(
            "replacement slot refers to unknown target {:?}",
            slot.current
        );
    }
    let mut result: Vec<Option<usize>> = vec![None; slots.len()];
    let mut counts = BTreeMap::<(u32, &str), usize>::new();
    let mut load = vec![0u64; domains.len()];
    for (i, slot) in slots.iter().enumerate() {
        let Some(current) = slot.current else {
            continue;
        };
        let count = counts
            .entry((slot.group, domains[current].as_str()))
            .or_default();
        if *count < bound {
            *count += 1;
            load[current] = load[current].saturating_add(slot.size);
            result[i] = Some(current);
        }
    }
    let mut moving: Vec<usize> = (0..slots.len()).filter(|&i| result[i].is_none()).collect();
    moving.sort_by_key(|&i| (slots[i].group, std::cmp::Reverse(slots[i].size), i));
    let mut cursor = 0usize;
    for i in moving {
        let slot = &slots[i];
        let chosen = (0..domains.len())
            .map(|n| (cursor + n) % domains.len())
            .min_by_key(|&j| {
                let count = counts
                    .get(&(slot.group, domains[j].as_str()))
                    .copied()
                    .unwrap_or(0);
                (count, load[j])
            })
            .expect("targets are not empty");
        let count = counts
            .entry((slot.group, domains[chosen].as_str()))
            .or_default();
        if *count >= bound && strict {
            bail!(
                "resilient relocation impossible: group {} would exceed {bound} shard(s) in one outage group; add independent target remotes or change K/M",
                slot.group
            );
        }
        *count += 1;
        load[chosen] = load[chosen].saturating_add(slot.size);
        result[i] = Some(chosen);
        cursor = (chosen + 1) % domains.len();
    }
    Ok(result
        .into_iter()
        .map(|slot| slot.expect("every slot assigned"))
        .collect())
}

fn ensure_parity_bound(
    targets: &[String],
    specs: &[PhysicalSpec],
    assignments: &[usize],
    parity: usize,
) -> Result<()> {
    if parity == 0 && specs.iter().all(|s| s.size == 0) {
        return Ok(()); // Empty files have no data bytes to protect.
    }
    if parity == 0 {
        bail!("resilient placement requires parity_shards > 0");
    }
    let mut counts = BTreeMap::<(u32, &str), usize>::new();
    for (spec, index) in specs.iter().zip(assignments) {
        let count = counts.entry((spec.group, &targets[*index])).or_default();
        *count += 1;
        if *count > parity {
            bail!("resilient placement impossible: group {} exceeds {} shards on one resolved backing target; add independent targets or change K/M", spec.group, parity);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(group: u32, current: Option<usize>) -> ReplacementSlot {
        ReplacementSlot {
            group,
            size: 10,
            current,
        }
    }
    fn domains(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn replacement_keeps_valid_slots_and_moves_only_lost_ones() {
        // 2+1 on a,b,c; c removed (current None) and d added.
        let targets = domains(&["a", "b", "d"]);
        let slots = [slot(0, Some(0)), slot(0, Some(1)), slot(0, None)];
        assert_eq!(
            assign_replacement_slots(&targets, &slots, 1, true).unwrap(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn replacement_evicts_kept_shards_that_break_the_outage_bound() {
        // Targets 0 and 1 share outage group "x": only one shard per group may stay.
        let targets = domains(&["x", "x", "y", "z"]);
        let slots = [slot(0, Some(0)), slot(0, Some(1)), slot(0, Some(2))];
        let assigned = assign_replacement_slots(&targets, &slots, 1, true).unwrap();
        assert_eq!(assigned[0], 0);
        assert_eq!(assigned[2], 2);
        assert_eq!(assigned[1], 3);
    }

    #[test]
    fn strict_replacement_refuses_to_exceed_the_bound() {
        let targets = domains(&["x", "x"]);
        let slots = [slot(0, Some(0)), slot(0, None)];
        assert!(assign_replacement_slots(&targets, &slots, 1, true).is_err());
        // Non-strict placements accept the best available layout.
        assert_eq!(
            assign_replacement_slots(&targets, &slots, 1, false).unwrap()[0],
            0
        );
    }

    #[test]
    fn replacement_spreads_moves_by_group_count_then_load() {
        let targets = domains(&["a", "b", "c", "d"]);
        let slots: Vec<_> = (0..6).map(|i| slot(i / 3, None)).collect();
        let assigned = assign_replacement_slots(&targets, &slots, 1, true).unwrap();
        for group in 0..2 {
            let used: BTreeSet<_> = assigned[group * 3..group * 3 + 3].iter().collect();
            assert_eq!(used.len(), 3);
        }
        let mut per_target = BTreeMap::new();
        for index in &assigned {
            *per_target.entry(index).or_insert(0) += 1;
        }
        assert!(per_target.values().all(|count| *count <= 2));
        assert!(assign_replacement_slots(&[], &slots, 1, true).is_err());
        assert!(assign_replacement_slots(&targets, &[slot(0, Some(9))], 1, true).is_err());
    }
    fn snapshot(free: &[u64]) -> crate::storage::admin::budget::BudgetSnapshot {
        crate::storage::admin::budget::BudgetSnapshot {
            targets: free
                .iter()
                .enumerate()
                .map(|(i, &free)| crate::storage::admin::budget::TargetBudget {
                    remote: format!("r{i}:"),
                    backing: format!("r{i}"),
                    capacity_domain: format!("q{i}"),
                    failure_domain: Some(format!("d{i}")),
                    declared: true,
                    total: 1000,
                    free,
                })
                .collect(),
            rejected: vec![],
            observed_targets: vec![],
        }
    }
    #[test]
    fn resilient_skips_full_targets_without_weakening_outage_bound() {
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 3];
        let assigned = assign_with_budget(
            &snapshot(&[0, 10, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1),
        )
        .unwrap();
        assert!(!assigned.contains(&0));
        assert_eq!(assigned.iter().collect::<BTreeSet<_>>().len(), 3);
        assert!(assign_with_budget(
            &snapshot(&[0, 0, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_err());
    }
    #[test]
    fn capacity_first_spends_large_account_without_claiming_outage_safety() {
        let gib = 1u64 << 30;
        let tib = 1u64 << 40;
        let mut s = snapshot(&[2 * tib, 2 * gib, 2 * gib, 2 * gib]);
        s.targets[0].total = 4 * tib; // Partly used; small accounts are empty.
        for target in &mut s.targets[1..] {
            target.total = target.free;
        }
        for target in &mut s.targets {
            target.failure_domain = None;
        }
        let specs = vec![
            PhysicalSpec {
                group: 0,
                size: gib
            };
            4
        ];
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_err());
        assert_eq!(
            assign_with_budget(&s, &specs, Placement::CapacityFirst, Some(1)).unwrap(),
            vec![0; 4]
        );
        // Multiple aliases never multiply a single account's actual quota.
        let mut aliases = snapshot(&[2 * gib, 2 * gib]);
        aliases.targets[1].capacity_domain = aliases.targets[0].capacity_domain.clone();
        assert!(assign_with_budget(&aliases, &specs, Placement::CapacityFirst, Some(1)).is_err());
    }
    #[test]
    fn two_choice_prefers_lower_projected_utilization_not_raw_free_bytes() {
        let mut s = snapshot(&[70, 500]);
        s.targets[0].total = 100;
        s.targets[1].total = 1000;
        let specs = [PhysicalSpec { group: 0, size: 10 }];
        assert_eq!(
            assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).unwrap(),
            vec![0]
        );
    }

    #[test]
    fn weighted_fallback_avoids_tiny_domain_when_large_domains_suffice() {
        let gib = 1u64 << 30;
        let tib = 1u64 << 40;
        let mut s = snapshot(&[2 * gib, 2 * tib, 2 * tib, 2 * tib, 2 * tib]);
        for target in &mut s.targets {
            target.total = target.free;
        }
        let specs: Vec<_> = (0..4)
            .flat_map(|group| {
                (0..4).map(move |_| PhysicalSpec {
                    group,
                    size: 1 << 20,
                })
            })
            .collect();
        let assigned = resilient_allocate_inner(&s, &specs, 1, ResilientChoice::Weighted).unwrap();
        assert!(!assigned.contains(&0));
        for group in 0..4 {
            let stripe = &assigned[(group * 4)..((group + 1) * 4)];
            assert_eq!(stripe.iter().copied().collect::<BTreeSet<_>>().len(), 4);
        }
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_ok());
    }

    #[test]
    fn two_choice_is_repeatable_and_preserves_nine_plus_three_outage_bound() {
        let s = snapshot(&[100; 7]);
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 12];
        let first = assign_with_budget(&s, &specs, Placement::Resilient, Some(3)).unwrap();
        assert_eq!(
            first,
            assign_with_budget(&s, &specs, Placement::Resilient, Some(3)).unwrap()
        );
        let mut counts = BTreeMap::new();
        for index in first {
            *counts
                .entry(s.targets[index].failure_domain.as_ref().unwrap())
                .or_insert(0) += 1;
        }
        assert!(counts.values().all(|count| *count <= 3));
    }

    #[test]
    fn two_choice_samples_distinct_quota_domains_and_keeps_group_prefix() {
        let mut aliases = snapshot(&[20, 20, 20, 90]);
        for target in &mut aliases.targets {
            target.total = 100;
        }
        for target in &mut aliases.targets[..3] {
            target.capacity_domain = "shared-account".into();
        }
        let one = [PhysicalSpec { group: 0, size: 10 }];
        assert_eq!(
            assign_with_budget(&aliases, &one, Placement::Resilient, Some(1)).unwrap(),
            vec![3]
        );

        let s = snapshot(&[100; 7]);
        let first_group = vec![PhysicalSpec { group: 0, size: 10 }; 3];
        let mut two_groups = first_group.clone();
        two_groups.extend(vec![PhysicalSpec { group: 1, size: 10 }; 3]);
        let prefix = assign_with_budget(&s, &first_group, Placement::Resilient, Some(1)).unwrap();
        let whole = assign_with_budget(&s, &two_groups, Placement::Resilient, Some(1)).unwrap();
        assert_eq!(prefix, whole[..3]);
    }

    #[test]
    fn resilient_shares_account_budget_and_outage_counts_across_aliases() {
        let specs = vec![PhysicalSpec { group: 0, size: 10 }; 3];
        let mut s = snapshot(&[10, 10, 10]);
        s.targets[1].capacity_domain = s.targets[0].capacity_domain.clone();
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_err());
        let mut s = snapshot(&[20, 20, 20]);
        s.targets[1].failure_domain = s.targets[0].failure_domain.clone();
        assert!(assign_with_budget(&s, &specs, Placement::Resilient, Some(1)).is_err());
    }
    #[test]
    fn resilient_budget_is_cumulative_and_partial_group_places_large_shards_first() {
        let specs: Vec<_> = (0..6)
            .map(|i| PhysicalSpec {
                group: i / 3,
                size: 10,
            })
            .collect();
        assert!(assign_with_budget(
            &snapshot(&[10, 10, 10]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_err());
        assert!(assign_with_budget(
            &snapshot(&[20, 20, 20]),
            &specs,
            Placement::Resilient,
            Some(1)
        )
        .is_ok());
        let partial = [
            PhysicalSpec { group: 0, size: 1 },
            PhysicalSpec { group: 0, size: 10 },
        ];
        let assigned =
            assign_with_budget(&snapshot(&[10, 1]), &partial, Placement::Resilient, Some(1))
                .unwrap();
        assert_eq!(assigned, vec![1, 0]);
        assert!(assign_with_budget(
            &snapshot(&[0]),
            &[PhysicalSpec { group: 0, size: 0 }],
            Placement::Resilient,
            None
        )
        .is_ok());
    }

    #[test]
    fn resumed_strict_plan_revalidates_changed_backing_aliases() {
        let mut plan = crate::planning::build_upload_plan(
            "unused",
            8,
            4,
            "archive".into(),
            vec!["x:".into(), "y:".into(), "z:".into()],
            Placement::RoundRobin,
            Some(Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: 2,
                parity_shards: 1,
                stripe_size: 2,
            }),
        )
        .unwrap();
        plan.placement = Placement::Resilient;
        let catalog = |collapse| {
            let mut catalog = crate::storage::admin::RemoteCatalog::parse(&serde_json::json!({
                "a":{"type":"s3"}, "b":{"type":"s3"}, "c":{"type":"s3"},
                "x":{"type":"crypt","remote":"a:"},
                "y":{"type":"crypt","remote": if collapse {"a:"} else {"b:"}},
                "z":{"type":"crypt","remote":"c:"}
            }))
            .unwrap();
            catalog.set_domains(crate::storage::admin::domains::DomainStore {
                version: 1,
                remotes: ["a", "b", "c"]
                    .into_iter()
                    .map(|name| {
                        (
                            name.into(),
                            crate::storage::admin::domains::DomainIdentity {
                                capacity: name.into(),
                                failure: name.into(),
                            },
                        )
                    })
                    .collect(),
            });
            catalog
        };
        assert!(validate_resilient_catalog(&catalog(false), &plan).is_ok());
        assert!(validate_resilient_catalog(&catalog(true), &plan).is_err());
    }
    #[test]
    fn resilient_rejects_impossible_layout_and_collapsed_aliases() {
        assert!(ensure_parity_bound(
            &["a".into()],
            &[PhysicalSpec { group: 0, size: 0 }],
            &[0],
            0
        )
        .is_ok());
        let specs = vec![PhysicalSpec { group: 0, size: 1 }; 10];
        for (targets, valid) in [
            (vec!["a", "b", "c", "d", "e"], true),
            (vec!["a", "a", "b", "c", "d"], false),
            (vec!["a", "b", "c"], false),
        ] {
            let targets: Vec<String> = targets.into_iter().map(str::to_owned).collect();
            let assigned = balanced_assign(&targets, &specs).unwrap();
            assert_eq!(
                ensure_parity_bound(&targets, &specs, &assigned, 2).is_ok(),
                valid
            );
            assert!(ensure_parity_bound(&targets, &specs, &assigned, 0).is_err());
        }
    }
    #[test]
    fn repeated_paths_cannot_weight_one_target_and_final_groups_balance() {
        let targets = vec!["a".into(), "a".into(), "b".into(), "c".into()];
        let specs: Vec<_> = (0..13)
            .map(|i| PhysicalSpec {
                group: if i < 10 { 0 } else { 1 },
                size: 1,
            })
            .collect();
        let assignments = balanced_assign(&targets, &specs).unwrap();
        for group in [0, 1] {
            let mut counts = BTreeMap::new();
            for (spec, index) in specs.iter().zip(&assignments) {
                if spec.group == group {
                    *counts.entry(&targets[*index]).or_insert(0) += 1;
                }
            }
            assert_eq!(counts.len(), 3);
            assert!(counts.values().max().unwrap() - counts.values().min().unwrap() <= 1);
        }
    }
}
