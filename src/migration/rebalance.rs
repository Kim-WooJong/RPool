//! Rebalance (opt-in, `pool migrate plan --rebalance`): chooses shard moves
//! that repair account limits; with the even-spread method it then spreads
//! each coding group's data shards evenly over the accounts (reads fetch data
//! shards first, so this is what makes them parallel); last it closes
//! sufficiently large free-ratio gaps without undoing that spread, e.g. after
//! adding an account. It does not replay fresh-upload placement; use
//! `--reapply-placement` for that. Planning only: moves are stored per entry and the run
//! relocates exactly those shards to exactly those remotes.
//!
//! One shared state walks the archives in planning order, so each decision
//! sees the quota left by the earlier ones. A moved shard credits its old
//! account; that space is free only after the original is retired.
use crate::prelude::*;
use crate::storage::admin::budget::TargetBudget;

/// Free-ratio gap (share of an account's size) below which a shard stays.
pub(crate) const MIN_GAP: f64 = 0.05;

#[derive(Debug)]
/// One target remote as the rebalancer sees it.
struct Account {
    /// Target remote (`name:path`) shards may move to.
    remote: String,
    /// Shard-count key: outage group (Resilient) or backing account.
    key: String,
    /// Quota domain whose free bytes are shared by remotes on one account.
    capacity_domain: String,
    /// Account size in bytes (at least 1), the free-ratio denominator.
    total: u64,
    /// This PC's lower limit of shards per coding group on this account
    /// (`AccountLimits::max_group_shards`); `None` = the placement's own.
    shard_cap: Option<usize>,
}

#[derive(Debug)]
/// Shared rebalance state for one plan: built once by the planner when
/// `PlanOptions::rebalance` is on, then asked per archive via `plan`.
pub(crate) struct Rebalancer {
    /// Target pool placement; decides the per-key shard cap.
    placement: Placement,
    /// Target remotes in pool order.
    accounts: Vec<Account>,
    /// Free bytes left per capacity domain, debited as moves are chosen.
    budgets: Mutex<BTreeMap<String, u64>>,
    /// Free bytes per capacity domain before any move (for the preview).
    initial: BTreeMap<String, u64>,
    /// Shards per account key before and after the planned moves.
    tally: Mutex<BTreeMap<String, Tally>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
/// Data and parity shards one account key holds, before and after.
pub(crate) struct Tally {
    /// Data shards before the moves.
    pub data_before: usize,
    /// Data shards after the moves.
    pub data_after: usize,
    /// Parity shards before the moves.
    pub parity_before: usize,
    /// Parity shards after the moves.
    pub parity_after: usize,
}

/// Moves chosen for one archive: shard index -> destination remote.
pub(crate) type Moves = BTreeMap<u32, String>;

impl Rebalancer {
    /// Every target remote needs a quota (and, for Resilient, a declared
    /// outage group); otherwise rebalancing would guess.
    pub(crate) fn new(
        target: &PoolDefinition,
        quotas: &[TargetBudget],
        failure_domain: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let name = |r: &str| r.split_once(':').map_or(r, |(n, _)| n).to_owned();
        let mut accounts = Vec::new();
        let mut budgets = BTreeMap::<String, u64>::new();
        for remote in &target.remotes {
            let quota = quotas
                .iter()
                .find(|q| &q.remote == remote)
                .or_else(|| quotas.iter().find(|q| name(&q.remote) == name(remote)))
                .with_context(|| format!("quota of {remote} is unknown"))?;
            let key = if target.placement == Placement::Resilient {
                failure_domain(remote)
                    .with_context(|| format!("outage group of {remote} is not declared"))?
            } else {
                quota.backing.clone()
            };
            budgets
                .entry(quota.capacity_domain.clone())
                .and_modify(|f| *f = (*f).min(quota.free))
                .or_insert(quota.free);
            accounts.push(Account {
                remote: remote.clone(),
                key,
                capacity_domain: quota.capacity_domain.clone(),
                total: quota.total.max(1),
                shard_cap: quota.shard_cap,
            });
        }
        if accounts.is_empty() {
            bail!("the pool has no remotes");
        }
        Ok(Self {
            placement: target.placement,
            accounts,
            initial: budgets.clone(),
            budgets: Mutex::new(budgets),
            tally: Mutex::new(BTreeMap::new()),
        })
    }

    /// Shards per key a coding group of `size` may hold, or None for no limit.
    fn cap(&self, size: usize, parity: usize) -> Option<usize> {
        let keys = self
            .accounts
            .iter()
            .map(|a| a.key.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            .max(1);
        let even = size.div_ceil(keys);
        match self.placement {
            Placement::RoundRobin | Placement::RoundRobinProtected => Some(even),
            Placement::FreeRatio | Placement::CapacityFirstProtected => {
                (parity > 0).then(|| parity.max(even))
            }
            // Even spread: never above the even share (protected or not;
            // the even share is within max(M, even)).
            Placement::ProportionalProtected => Some(even),
            Placement::FreeRatioUnbounded => None,
            Placement::Resilient => (parity > 0).then_some(parity),
            // Spread each coding group across the accounts for read
            // parallelism (no durability cap): hold at most ceil(size/keys),
            // so an account over its share gives the excess to emptier ones.
            // Bigger accounts still take more only once smaller ones fill.
            Placement::Proportional => Some(even),
            Placement::CapacityFirst => None,
        }
    }

    /// Moves for `manifest`. `forced` shards must move anyway (removed
    /// remotes, outage-bound excess); they get a destination here too when one
    /// fits. Returns the moves and human-readable reasons.
    pub(crate) fn plan(&self, manifest: &Manifest, forced: &BTreeSet<u32>) -> (Moves, Vec<String>) {
        let parity = manifest.coding.as_ref().map_or(0, |c| c.parity_shards);
        let mut budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let position = |remote: &str| self.accounts.iter().position(|a| a.remote == remote);
        let mut groups = BTreeMap::<u32, Vec<usize>>::new();
        for (i, shard) in manifest.shards.iter().enumerate() {
            groups.entry(shard.group).or_default().push(i);
        }
        let mut moves = Moves::new();
        let (mut placed, mut capped, mut spread, mut balanced) = (0usize, 0usize, 0usize, 0usize);
        for members in groups.values() {
            let cap = self.cap(members.len(), parity);
            let mut at: BTreeMap<usize, Option<usize>> = members
                .iter()
                .map(|&i| {
                    let shard = &manifest.shards[i];
                    let here = position(&shard.remote).filter(|_| !forced.contains(&shard.index));
                    (i, here)
                })
                .collect();
            let mut counts = BTreeMap::<String, usize>::new();
            for account in at.values().flatten() {
                *counts
                    .entry(self.accounts[*account].key.clone())
                    .or_default() += 1;
            }
            let ratio = |budgets: &BTreeMap<String, u64>, a: usize| {
                let account = &self.accounts[a];
                budgets[&account.capacity_domain] as f64 / account.total as f64
            };
            // Best destination for `size` bytes, never onto `exclude`'s key.
            let best = |budgets: &BTreeMap<String, u64>,
                        counts: &BTreeMap<String, usize>,
                        size: u64,
                        exclude: Option<&str>| {
                (0..self.accounts.len())
                    .filter(|&a| {
                        let account = &self.accounts[a];
                        let count = counts.get(&account.key).copied().unwrap_or(0);
                        budgets[&account.capacity_domain] >= size
                            && cap.is_none_or(|m| count < m)
                            && account.shard_cap.is_none_or(|m| count < m)
                            && exclude != Some(account.key.as_str())
                    })
                    .max_by(|&a, &b| {
                        ratio(budgets, a)
                            .total_cmp(&ratio(budgets, b))
                            .then(b.cmp(&a))
                    })
            };
            // 1. Shards that must leave their remote.
            for &i in members {
                if at[&i].is_some() {
                    continue;
                }
                let size = manifest.shards[i].size;
                if let Some(to) = best(&budgets, &counts, size, None) {
                    self.apply(
                        manifest,
                        &mut budgets,
                        &mut counts,
                        &mut at,
                        &mut moves,
                        i,
                        to,
                    );
                    placed += 1;
                }
            }
            // 2. Keys over either the placement or an explicit account limit
            // give up their excess shards. This also applies when the policy
            // itself has no outage cap.
            for &i in members {
                let Some(from) = at[&i] else { continue };
                let Some(limit) = [cap, self.accounts[from].shard_cap]
                    .into_iter()
                    .flatten()
                    .min()
                else {
                    continue;
                };
                let key = self.accounts[from].key.clone();
                if counts.get(&key).copied().unwrap_or(0) <= limit
                    || moves.contains_key(&manifest.shards[i].index)
                {
                    continue;
                }
                if let Some(to) = best(&budgets, &counts, manifest.shards[i].size, Some(&key)) {
                    self.apply(
                        manifest,
                        &mut budgets,
                        &mut counts,
                        &mut at,
                        &mut moves,
                        i,
                        to,
                    );
                    capped += 1;
                }
            }
            // 2b. Read spread: data shards evenly over the account keys.
            let spreads = self.spreads_data();
            if spreads {
                spread += self.spread_data(
                    manifest,
                    members,
                    cap,
                    &mut budgets,
                    &mut counts,
                    &mut at,
                    &mut moves,
                );
            }
            // 3. Free-ratio balance: try the fullest source with a feasible
            //    destination. A rejected pair must not hide another move, and
            //    a data shard never moves to a key holding as many data shards
            //    as its source, so the read spread of 2b stays.
            if self.placement.method() == PlacementMethod::RoundRobin {
                continue;
            }
            loop {
                let choice = members
                    .iter()
                    .copied()
                    .filter(|&i| !moves.contains_key(&manifest.shards[i].index))
                    .filter_map(|i| {
                        let from = at[&i]?;
                        let size = manifest.shards[i].size;
                        let src = &self.accounts[from];
                        let src_free = budgets[&src.capacity_domain];
                        let data = self.data_by_key(manifest, members, &at);
                        let is_data = spreads && manifest.shards[i].kind == ShardKind::Data;
                        let to = (0..self.accounts.len())
                            .filter(|&to| {
                                let dst = &self.accounts[to];
                                let count = counts.get(&dst.key).copied().unwrap_or(0);
                                let dst_free = budgets[&dst.capacity_domain];
                                dst_free >= size
                                    && cap.is_none_or(|m| count < m)
                                    && dst.shard_cap.is_none_or(|m| count < m)
                                    && src.key != dst.key
                                    && src.capacity_domain != dst.capacity_domain
                                    && (!is_data || data[&dst.key] + 1 < data[&src.key])
                                    && ratio(&budgets, to) - ratio(&budgets, from) > MIN_GAP
                                    && (dst_free - size) as f64 / dst.total as f64
                                        >= (src_free + size) as f64 / src.total as f64
                            })
                            .max_by(|&a, &b| {
                                ratio(&budgets, a)
                                    .total_cmp(&ratio(&budgets, b))
                                    .then(b.cmp(&a))
                            })?;
                        Some((i, from, to))
                    })
                    .min_by(|x, y| {
                        ratio(&budgets, x.1)
                            .total_cmp(&ratio(&budgets, y.1))
                            .then(x.0.cmp(&y.0))
                    });
                let Some((i, _, to)) = choice else { break };
                self.apply(
                    manifest,
                    &mut budgets,
                    &mut counts,
                    &mut at,
                    &mut moves,
                    i,
                    to,
                );
                balanced += 1;
            }
        }
        let mut reasons = Vec::new();
        if placed > 0 {
            reasons.push(format!(
                "rebalance: {placed} moving shard(s) placed by free ratio"
            ));
        }
        if capped > 0 {
            reasons.push(format!(
                "rebalance: {capped} shard(s) over a per-account shard limit"
            ));
        }
        if spread > 0 {
            reasons.push(format!(
                "rebalance: {spread} shard move(s) spreading data shards evenly for read speed"
            ));
        }
        if balanced > 0 {
            reasons.push(format!(
                "rebalance: {balanced} shard(s) moved to accounts with more free space"
            ));
        }
        drop(budgets);
        self.record(manifest, &moves);
        (moves, reasons)
    }

    /// The even-spread method spreads data shards (read speed first); the
    /// other methods keep their own layout.
    fn spreads_data(&self) -> bool {
        self.placement.spreads_data()
    }

    /// Data shards of the group `members` per account key at their current
    /// (planned) place; every key is present.
    fn data_by_key(
        &self,
        manifest: &Manifest,
        members: &[usize],
        at: &BTreeMap<usize, Option<usize>>,
    ) -> BTreeMap<String, usize> {
        let mut data: BTreeMap<String, usize> =
            self.accounts.iter().map(|a| (a.key.clone(), 0)).collect();
        for &i in members {
            if manifest.shards[i].kind != ShardKind::Data {
                continue;
            }
            if let Some(a) = at[&i] {
                *data.entry(self.accounts[a].key.clone()).or_default() += 1;
            }
        }
        data
    }

    /// Evens out the group's data shards over the account keys until no key
    /// holds two more than another. A data shard moves to a key with room;
    /// when that key is at its shard limit it swaps places with one of the
    /// key's parity shards, so every key keeps its shard count (and nearly
    /// its bytes). Only shards not moved yet are picked. Returns the moves.
    #[allow(clippy::too_many_arguments)]
    fn spread_data(
        &self,
        manifest: &Manifest,
        members: &[usize],
        cap: Option<usize>,
        budgets: &mut BTreeMap<String, u64>,
        counts: &mut BTreeMap<String, usize>,
        at: &mut BTreeMap<usize, Option<usize>>,
        moves: &mut Moves,
    ) -> usize {
        let ratio = |budgets: &BTreeMap<String, u64>, a: usize| {
            let account = &self.accounts[a];
            budgets[&account.capacity_domain] as f64 / account.total as f64
        };
        let unmoved = |moves: &Moves, at: &BTreeMap<usize, Option<usize>>, i: usize, key: &str| {
            !moves.contains_key(&manifest.shards[i].index)
                && at[&i].is_some_and(|a| self.accounts[a].key == key)
        };
        let mut made = 0;
        loop {
            let data = self.data_by_key(manifest, members, at);
            let mut keys: Vec<(&String, usize)> = data.iter().map(|(k, n)| (k, *n)).collect();
            keys.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let mut step: Option<Vec<(usize, usize)>> = None;
            'pairs: for &(hi, hn) in &keys {
                for &(lo, ln) in keys.iter().rev() {
                    if hn < ln + 2 {
                        continue;
                    }
                    // Data shards on `hi`, fullest account first.
                    let mut sources: Vec<usize> = members
                        .iter()
                        .copied()
                        .filter(|&i| manifest.shards[i].kind == ShardKind::Data)
                        .filter(|&i| unmoved(moves, at, i, hi))
                        .collect();
                    sources.sort_by(|&x, &y| {
                        let (ax, ay) = (at[&x].unwrap_or(0), at[&y].unwrap_or(0));
                        ratio(budgets, ax)
                            .total_cmp(&ratio(budgets, ay))
                            .then(x.cmp(&y))
                    });
                    let lo_count = counts.get(lo).copied().unwrap_or(0);
                    for &i in &sources {
                        let size = manifest.shards[i].size;
                        // Direct move onto an account of `lo` with room.
                        let direct = (0..self.accounts.len())
                            .filter(|&a| {
                                let account = &self.accounts[a];
                                &account.key == lo
                                    && budgets[&account.capacity_domain] >= size
                                    && cap.is_none_or(|m| lo_count < m)
                                    && account.shard_cap.is_none_or(|m| lo_count < m)
                            })
                            .max_by(|&a, &b| {
                                ratio(budgets, a)
                                    .total_cmp(&ratio(budgets, b))
                                    .then(b.cmp(&a))
                            });
                        if let Some(to) = direct {
                            step = Some(vec![(i, to)]);
                            break 'pairs;
                        }
                        // Swap with a parity shard of `lo`.
                        let Some(from) = at[&i] else { continue };
                        let swap = members
                            .iter()
                            .copied()
                            .filter(|&j| manifest.shards[j].kind == ShardKind::Parity)
                            .filter(|&j| unmoved(moves, at, j, lo))
                            .find(|&j| {
                                let Some(other) = at[&j] else { return false };
                                let (src, dst) = (&self.accounts[from], &self.accounts[other]);
                                let parity_size = manifest.shards[j].size;
                                budgets[&src.capacity_domain] >= parity_size
                                    && budgets[&dst.capacity_domain] + parity_size >= size
                            });
                        if let Some(j) = swap {
                            let other = at[&j].unwrap_or(from);
                            step = Some(vec![(j, from), (i, other)]);
                            break 'pairs;
                        }
                    }
                }
            }
            let Some(step) = step else { break };
            for (i, to) in step {
                self.apply(manifest, budgets, counts, at, moves, i, to);
                made += 1;
            }
        }
        made
    }

    /// Adds this archive's shards before/after its moves to the preview.
    fn record(&self, manifest: &Manifest, moves: &Moves) {
        let key_of = |remote: &str| {
            self.accounts
                .iter()
                .find(|a| a.remote == remote)
                .map(|a| a.key.clone())
        };
        let mut tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        for shard in &manifest.shards {
            let data = shard.kind == ShardKind::Data;
            if let Some(key) = key_of(&shard.remote) {
                let t = tally.entry(key).or_default();
                if data {
                    t.data_before += 1
                } else {
                    t.parity_before += 1
                }
            }
            let now = moves
                .get(&shard.index)
                .map_or(shard.remote.as_str(), String::as_str);
            if let Some(key) = key_of(now) {
                let t = tally.entry(key).or_default();
                if data {
                    t.data_after += 1
                } else {
                    t.parity_after += 1
                }
            }
        }
    }

    /// Per-account preview of the planned rebalance: data/parity shards and
    /// free-space share before and after (after = once the originals are
    /// retired), for the plan notes of the CLI and GUI.
    pub(crate) fn summary(&self) -> Vec<String> {
        let tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        let budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines = vec![
            "Rebalance preview per account (shards of the archives rebalancing looked at; free share after the originals are retired):".to_owned(),
        ];
        let mut seen = BTreeSet::new();
        for account in &self.accounts {
            if !seen.insert(account.key.clone()) {
                continue;
            }
            let t = tally.get(&account.key).copied().unwrap_or_default();
            let share = |free: u64| 100.0 * free as f64 / account.total as f64;
            let before = self
                .initial
                .get(&account.capacity_domain)
                .copied()
                .unwrap_or(0);
            let after = budgets.get(&account.capacity_domain).copied().unwrap_or(0);
            lines.push(format!(
                "  {}: data shards {} -> {}, parity shards {} -> {}, free {:.1}% -> {:.1}%",
                account.key,
                t.data_before,
                t.data_after,
                t.parity_before,
                t.parity_after,
                share(before),
                share(after),
            ));
        }
        lines
    }

    /// Records the move of shard `i` to account `to`.
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        manifest: &Manifest,
        budgets: &mut BTreeMap<String, u64>,
        counts: &mut BTreeMap<String, usize>,
        at: &mut BTreeMap<usize, Option<usize>>,
        moves: &mut Moves,
        i: usize,
        to: usize,
    ) {
        let shard = &manifest.shards[i];
        if let Some(from) = at[&i] {
            if let Some(count) = counts.get_mut(&self.accounts[from].key) {
                *count -= 1;
            }
        }
        // The old copy's space comes back once the original is retired.
        if let Some(old) = self.accounts.iter().find(|a| a.remote == shard.remote) {
            if let Some(free) = budgets.get_mut(&old.capacity_domain) {
                *free += shard.size;
            }
        }
        let account = &self.accounts[to];
        if let Some(free) = budgets.get_mut(&account.capacity_domain) {
            *free = free.saturating_sub(shard.size);
        }
        *counts.entry(account.key.clone()).or_default() += 1;
        at.insert(i, Some(to));
        moves.insert(shard.index, account.remote.clone());
    }
}

#[cfg(test)]
#[path = "rebalance_tests.rs"]
mod tests;
