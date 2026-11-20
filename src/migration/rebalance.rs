//! Logical, after-retirement rebalance scoring, NOT a physical write budget.
//! Forced moves, limit repair, read spread, then capacity balance run in that
//! order. `storage_budget` must independently admit every created output while
//! originals remain. A logical source credit never authorizes a physical write.
use crate::prelude::*;
use crate::storage::admin::budget::TargetBudget;

pub(crate) const MIN_GAP: f64 = 0.05;

#[derive(Debug)]
struct Account {
    remote: String,
    capacity_domain: String,
    /// Manual account limits and legacy protection count backing accounts.
    backing_key: String,
    /// Resilient protection counts outage domains, independently of backing.
    failure_key: Option<String>,
    /// Independent disks spread independently; capacity aliases count once.
    spread_key: String,
    total: u64,
    shard_cap: Option<usize>,
}

impl Account {
    fn count_keys(&self) -> impl Iterator<Item = &String> {
        [&self.backing_key, &self.spread_key]
            .into_iter()
            .chain(self.failure_key.iter())
    }
}

#[derive(Debug)]
pub(crate) struct Rebalancer {
    placement: Placement,
    accounts: Vec<Account>,
    /// Logical free bytes AFTER retiring originals, for scoring only.
    budgets: Mutex<BTreeMap<String, u64>>,
    initial: BTreeMap<String, u64>,
    tally: Mutex<BTreeMap<String, Tally>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub data_before: usize,
    pub data_after: usize,
    pub parity_before: usize,
    pub parity_after: usize,
}

pub(crate) type Moves = BTreeMap<u32, String>;
type Locations = BTreeMap<usize, Option<usize>>;
type Counts = BTreeMap<String, usize>;

impl Rebalancer {
    pub(crate) fn new(
        target: &PoolDefinition,
        quotas: &[TargetBudget],
        failure_domain: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let name = |r: &str| r.split_once(':').map_or(r, |(n, _)| n).to_owned();
        let mut accounts = Vec::new();
        let mut budgets = BTreeMap::<String, u64>::new();
        let mut totals = BTreeMap::new();
        for remote in &target.remotes {
            let quota = if let Some(exact) = quotas.iter().find(|q| &q.remote == remote) {
                exact
            } else {
                let mut aliases = quotas.iter().filter(|q| name(&q.remote) == name(remote));
                let first = aliases.next().with_context(|| format!("quota of {remote} is unknown"))?;
                if aliases.any(|q| q.capacity_domain != first.capacity_domain || q.backing != first.backing) {
                    bail!("quota identity of {remote} is ambiguous; an exact path quota is required");
                }
                first
            };
            if quota.capacity_domain.is_empty() || quota.backing.is_empty()
                || quota.total == 0 || quota.free > quota.total {
                bail!("invalid quota or identity of {remote}");
            }
            if totals.insert(quota.capacity_domain.clone(), quota.total)
                .is_some_and(|old| old != quota.total) {
                bail!("inconsistent quota total for {}", quota.capacity_domain);
            }
            let failure_key = if target.placement == Placement::Resilient {
                let domain = failure_domain(remote)
                    .filter(|d| !d.is_empty())
                    .with_context(|| format!("outage group of {remote} is not declared"))?;
                Some(format!("failure:{domain}"))
            } else { None };
            budgets.entry(quota.capacity_domain.clone())
                .and_modify(|f| *f = (*f).min(quota.free)).or_insert(quota.free);
            accounts.push(Account {
                remote: remote.clone(),
                capacity_domain: quota.capacity_domain.clone(),
                backing_key: format!("backing:{}", quota.backing),
                failure_key,
                spread_key: format!("spread:{}", quota.capacity_domain),
                total: quota.total,
                shard_cap: quota.shard_cap,
            });
        }
        if accounts.is_empty() { bail!("the pool has no remotes"); }
        Ok(Self { placement: target.placement, accounts, initial: budgets.clone(),
            budgets: Mutex::new(budgets), tally: Mutex::new(BTreeMap::new()) })
    }

    /// Independent bounds. Retain the existing rebalance policy's bounds:
    /// RR and protected spread use even/backing; free-ratio and protected
    /// capacity-first use max(M, even/backing); Resilient uses M/failure.
    /// Unprotected spread's even bound belongs to the spread identity, not
    /// the backing identity. Manual backing bounds always apply separately.
    fn limits(&self, size: usize, parity: usize) -> Counts {
        let backings = self.accounts.iter().map(|a| &a.backing_key)
            .collect::<BTreeSet<_>>().len().max(1);
        let disks = self.accounts.iter().map(|a| &a.spread_key)
            .collect::<BTreeSet<_>>().len().max(1);
        let even = size.div_ceil(backings);
        let protected = match self.placement {
            Placement::RoundRobin | Placement::RoundRobinProtected
                | Placement::ProportionalProtected => Some(even),
            Placement::FreeRatio | Placement::CapacityFirstProtected =>
                (parity > 0).then_some(parity.max(even)),
            _ => None,
        };
        let mut limits = Counts::new();
        let mut bound = |key: &String, limit: usize| {
            limits.entry(key.clone()).and_modify(|n| *n = (*n).min(limit))
                .or_insert(limit);
        };
        for a in &self.accounts {
            if let Some(n) = protected { bound(&a.backing_key, n); }
            if let Some(n) = a.shard_cap { bound(&a.backing_key, n); }
            if let Some(key) = &a.failure_key {
                if parity > 0 { bound(key, parity); }
            }
            if self.placement == Placement::Proportional {
                bound(&a.spread_key, size.div_ceil(disks));
            }
        }
        limits
    }

    fn counts(&self, at: &Locations) -> Counts {
        let mut counts = Counts::new();
        for a in at.values().flatten() {
            for key in self.accounts[*a].count_keys() {
                *counts.entry(key.clone()).or_default() += 1;
            }
        }
        counts
    }

    fn ratio(&self, budgets: &BTreeMap<String, u64>, a: usize) -> f64 {
        let account = &self.accounts[a];
        budgets[&account.capacity_domain] as f64 / account.total as f64
    }

    /// Validate a simultaneous transition: before - outgoing + incoming.
    /// An unchanged key at/over its cap is not a new violation. No transition
    /// may introduce or worsen a violation. Swaps use their FINAL counts.
    /// Logical arithmetic is checked; overflow is an infeasible candidate.
    fn transition(
        &self, manifest: &Manifest, at: &Locations, budgets: &BTreeMap<String, u64>,
        limits: &Counts, step: &[(usize, usize)],
    ) -> Option<(Locations, BTreeMap<String, u64>)> {
        let before = self.counts(at);
        let mut after = at.clone();
        let mut logical = budgets.clone();
        let mut seen = BTreeSet::new();
        for &(i, to) in step {
            if !seen.insert(i) { return None; }
            let shard = &manifest.shards[i];
            let dst = &self.accounts[to];
            // Same quota domain is not a capacity rebalance, even via aliases.
            if self.accounts.iter().any(|a| a.remote == shard.remote
                && a.capacity_domain == dst.capacity_domain) { return None; }
            if let Some(src) = self.accounts.iter().find(|a| a.remote == shard.remote) {
                let free = logical.get_mut(&src.capacity_domain)?;
                *free = free.checked_add(shard.size)?;
            }
            after.insert(i, Some(to));
        }
        for &(i, to) in step {
            let free = logical.get_mut(&self.accounts[to].capacity_domain)?;
            *free = free.checked_sub(manifest.shards[i].size)?;
        }
        let counts = self.counts(&after);
        if limits.iter().any(|(k, cap)| {
            counts.get(k).copied().unwrap_or(0) > (*cap).max(before.get(k).copied().unwrap_or(0))
        }) { return None; }
        Some((after, logical))
    }

    fn apply_logical(
        &self, manifest: &Manifest, at: &mut Locations, budgets: &mut BTreeMap<String, u64>,
        moves: &mut Moves, limits: &Counts, step: &[(usize, usize)],
    ) {
        // Callers select only transitions accepted against this exact state.
        if let Some((after, logical)) = self.transition(manifest, at, budgets, limits, step) {
            *at = after;
            *budgets = logical;
            for &(i, to) in step {
                moves.insert(manifest.shards[i].index, self.accounts[to].remote.clone());
            }
        }
    }

    pub(crate) fn plan(&self, manifest: &Manifest, forced: &BTreeSet<u32>) -> (Moves, Vec<String>) {
        let parity = manifest.coding.as_ref().map_or(0, |c| c.parity_shards);
        let mut budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let mut groups = BTreeMap::<u32, Vec<usize>>::new();
        for (i, shard) in manifest.shards.iter().enumerate() {
            groups.entry(shard.group).or_default().push(i);
        }
        let mut moves = Moves::new();
        let (mut placed, mut capped, mut spread, mut balanced) = (0, 0, 0, 0);
        let mut remaining_limits = 0;
        for members in groups.values() {
            let limits = self.limits(members.len(), parity);
            // Forced shards still count at their source until a move succeeds.
            let mut at: Locations = members.iter().map(|&i| (i, self.accounts.iter()
                .position(|a| a.remote == manifest.shards[i].remote))).collect();
            let best = |at: &Locations, budgets: &BTreeMap<String, u64>, i: usize, repair: bool| {
                let before = self.counts(at);
                (0..self.accounts.len()).filter(|&to| {
                    let step = [(i, to)];
                    let Some((next, _)) = self.transition(manifest, at, budgets, &limits, &step)
                        else { return false };
                    if !repair { return true; }
                    let after = self.counts(&next);
                    limits.iter().any(|(k, cap)| before.get(k).copied().unwrap_or(0) > *cap
                        && after.get(k).copied().unwrap_or(0) < before[k])
                }).max_by(|&a, &b| self.ratio(budgets, a)
                    .total_cmp(&self.ratio(budgets, b)).then(b.cmp(&a)))
            };
            // 1. Mandatory departures.
            for &i in members {
                if at[&i].is_some() && !forced.contains(&manifest.shards[i].index) { continue; }
                if let Some(to) = best(&at, &budgets, i, false) {
                    self.apply_logical(manifest, &mut at, &mut budgets, &mut moves, &limits, &[(i, to)]);
                    placed += 1;
                }
            }
            // 2. Repair a genuinely over-limit axis, without weakening others.
            for &i in members {
                if moves.contains_key(&manifest.shards[i].index) || at[&i].is_none() { continue; }
                if let Some(to) = best(&at, &budgets, i, true) {
                    self.apply_logical(manifest, &mut at, &mut budgets, &mut moves, &limits, &[(i, to)]);
                    capped += 1;
                }
            }
            // 3. Read spread (simultaneous data/parity swaps preserve limits).
            if self.placement.spreads_data() {
                spread += self.spread_data(manifest, members, &limits, &mut budgets, &mut at, &mut moves);
            }
            // 4. Whole-shard capacity balance, preserving data spread.
            if self.placement.method() != PlacementMethod::RoundRobin {
                loop {
                    let data = self.data_by_key(manifest, members, &at);
                    let choice = members.iter().copied()
                        .filter(|&i| !moves.contains_key(&manifest.shards[i].index))
                        .filter_map(|i| {
                            let from = at[&i]?;
                            let src = &self.accounts[from];
                            let is_data = self.placement.spreads_data() && manifest.shards[i].kind == ShardKind::Data;
                            let to = (0..self.accounts.len()).filter(|&to| {
                                let dst = &self.accounts[to];
                                if is_data && src.spread_key != dst.spread_key
                                    && data[&dst.spread_key] + 1 >= data[&src.spread_key] { return false; }
                                if self.ratio(&budgets, to) - self.ratio(&budgets, from) <= MIN_GAP { return false; }
                                let Some((_, next)) = self.transition(manifest, &at, &budgets, &limits, &[(i, to)])
                                    else { return false };
                                self.ratio(&next, to) >= self.ratio(&next, from)
                            }).max_by(|&a, &b| self.ratio(&budgets, a)
                                .total_cmp(&self.ratio(&budgets, b)).then(b.cmp(&a)))?;
                            Some((i, from, to))
                        }).min_by(|x, y| self.ratio(&budgets, x.1)
                            .total_cmp(&self.ratio(&budgets, y.1)).then(x.0.cmp(&y.0)));
                    let Some((i, _, to)) = choice else { break };
                    self.apply_logical(manifest, &mut at, &mut budgets, &mut moves, &limits, &[(i, to)]);
                    balanced += 1;
                }
            }
            let counts = self.counts(&at);
            remaining_limits += limits.iter().filter(|(k, cap)| counts.get(*k).copied().unwrap_or(0) > **cap).count();
        }
        let mut reasons = Vec::new();
        for (n, text) in [(placed, "moving shard(s) placed by free ratio"),
            (capped, "shard(s) over a per-account shard limit"),
            (spread, "shard move(s) spreading data shards evenly for read speed"),
            (balanced, "shard(s) moved to accounts with more free space")] {
            if n > 0 { reasons.push(format!("rebalance: {n} {text}")); }
        }
        let unresolved = manifest.shards.iter().filter(|shard| {
            (forced.contains(&shard.index) || !self.accounts.iter().any(|a| a.remote == shard.remote))
                && !moves.contains_key(&shard.index)
        }).count();
        if unresolved > 0 { reasons.push(format!("rebalance blocked: {unresolved} mandatory shard move(s) have no destination satisfying identity, logical capacity and shard limits")); }
        if remaining_limits > 0 { reasons.push(format!("rebalance blocked: {remaining_limits} group/domain limit(s) remain above their cap; no non-worsening whole-shard repair found")); }
        drop(budgets);
        self.record(manifest, &moves);
        (moves, reasons)
    }

    fn data_by_key(&self, manifest: &Manifest, members: &[usize], at: &Locations) -> Counts {
        let mut data: Counts = self.accounts.iter().map(|a| (a.spread_key.clone(), 0)).collect();
        for &i in members {
            if manifest.shards[i].kind == ShardKind::Data {
                if let Some(a) = at[&i] { *data.entry(self.accounts[a].spread_key.clone()).or_default() += 1; }
            }
        }
        data
    }

    fn spread_data(
        &self, manifest: &Manifest, members: &[usize], limits: &Counts,
        budgets: &mut BTreeMap<String, u64>, at: &mut Locations, moves: &mut Moves,
    ) -> usize {
        let mut made = 0;
        loop {
            let data = self.data_by_key(manifest, members, at);
            let mut keys: Vec<_> = data.iter().collect();
            keys.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let mut step = None;
            'pairs: for &(hi, hn) in &keys {
                for &(lo, ln) in keys.iter().rev() {
                    if *hn < *ln + 2 { continue; }
                    let mut sources: Vec<_> = members.iter().copied().filter(|&i| {
                        manifest.shards[i].kind == ShardKind::Data
                            && !moves.contains_key(&manifest.shards[i].index)
                            && at[&i].is_some_and(|a| &self.accounts[a].spread_key == hi)
                    }).collect();
                    sources.sort_by(|&x, &y| self.ratio(budgets, at[&x].unwrap_or(0))
                        .total_cmp(&self.ratio(budgets, at[&y].unwrap_or(0))).then(x.cmp(&y)));
                    for i in sources {
                        let direct = (0..self.accounts.len()).filter(|&to| {
                            &self.accounts[to].spread_key == lo
                                && self.transition(manifest, at, budgets, limits, &[(i, to)]).is_some()
                        }).max_by(|&a, &b| self.ratio(budgets, a)
                            .total_cmp(&self.ratio(budgets, b)).then(b.cmp(&a)));
                        if let Some(to) = direct { step = Some(vec![(i, to)]); break 'pairs; }
                        let Some(from) = at[&i] else { continue };
                        for &j in members {
                            if manifest.shards[j].kind != ShardKind::Parity
                                || moves.contains_key(&manifest.shards[j].index) { continue; }
                            let Some(other) = at[&j] else { continue };
                            if &self.accounts[other].spread_key != lo { continue; }
                            let swap = vec![(j, from), (i, other)];
                            if self.transition(manifest, at, budgets, limits, &swap).is_some() {
                                step = Some(swap); break 'pairs;
                            }
                        }
                    }
                }
            }
            let Some(step) = step else { break };
            self.apply_logical(manifest, at, budgets, moves, limits, &step);
            made += step.len();
        }
        made
    }

    fn record(&self, manifest: &Manifest, moves: &Moves) {
        let key_of = |remote: &str| self.accounts.iter().find(|a| a.remote == remote)
            .map(|a| a.capacity_domain.clone());
        let mut tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        for shard in &manifest.shards {
            let data = shard.kind == ShardKind::Data;
            if let Some(key) = key_of(&shard.remote) {
                let t = tally.entry(key).or_default();
                if data { t.data_before += 1; } else { t.parity_before += 1; }
            }
            let now = moves.get(&shard.index).map_or(shard.remote.as_str(), String::as_str);
            if let Some(key) = key_of(now) {
                let t = tally.entry(key).or_default();
                if data { t.data_after += 1; } else { t.parity_after += 1; }
            }
        }
    }

    pub(crate) fn summary(&self) -> Vec<String> {
        let tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        let budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines = vec!["Rebalance preview per capacity domain (logical free AFTER originals are retired; NOT peak write headroom):".to_owned()];
        let mut seen = BTreeSet::new();
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        for a in &self.accounts {
            if !seen.insert(&a.capacity_domain) { continue; }
            let t = tally.get(&a.capacity_domain).copied().unwrap_or_default();
            let before = self.initial[&a.capacity_domain];
            let after = budgets[&a.capacity_domain];
            let ratio = 100.0 * after as f64 / a.total as f64;
            min = min.min(ratio); max = max.max(ratio);
            lines.push(format!("  {}: data shards {} -> {}, parity shards {} -> {}, free {:.1}% -> {:.1}%",
                a.capacity_domain, t.data_before, t.data_after, t.parity_before, t.parity_after,
                100.0 * before as f64 / a.total as f64, ratio));
        }
        lines.push(format!("  residual logical free-ratio gap: {:.1} percentage points (whole-shard threshold {:.1})", max - min, MIN_GAP * 100.0));
        if max - min > MIN_GAP * 100.0 {
            lines.push("  remaining gap: whole-shard size/no-overshoot, shard limits, read spread or policy order can block further moves; physical admission is checked separately".into());
        }
        lines
    }
}

#[cfg(test)]
#[path = "rebalance_tests.rs"]
mod tests;
