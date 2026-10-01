//! Rebalance (opt-in, `pool migrate plan --rebalance`): chooses shard moves
//! that bring existing archives in line with the pool's current placement and
//! spread used space by free ratio, e.g. after adding an account or changing
//! the placement. Planning only: the moves are stored per entry and the run
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
struct Account {
    remote: String,
    /// Shard-count key: outage group (Resilient) or backing account.
    key: String,
    capacity_domain: String,
    total: u64,
}

#[derive(Debug)]
pub(crate) struct Rebalancer {
    placement: Placement,
    accounts: Vec<Account>,
    budgets: Mutex<BTreeMap<String, u64>>,
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
            });
        }
        if accounts.is_empty() {
            bail!("the pool has no remotes");
        }
        Ok(Self {
            placement: target.placement,
            accounts,
            budgets: Mutex::new(budgets),
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
            Placement::RoundRobin => Some(even),
            Placement::FreeRatio => (parity > 0).then(|| parity.max(even)),
            Placement::Resilient => (parity > 0).then_some(parity),
            Placement::Proportional | Placement::CapacityFirst => None,
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
        let (mut placed, mut capped, mut balanced) = (0usize, 0usize, 0usize);
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
            // 2. Keys over the placement's limit give up their excess shards.
            if let Some(m) = cap {
                for &i in members {
                    let Some(from) = at[&i] else { continue };
                    let key = self.accounts[from].key.clone();
                    if counts.get(&key).copied().unwrap_or(0) <= m
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
            }
            // 3. Free-ratio balance: move from the fullest account while the
            //    gap is real and the move does not overshoot.
            if self.placement == Placement::RoundRobin {
                continue;
            }
            loop {
                let source = members
                    .iter()
                    .copied()
                    .filter(|&i| !moves.contains_key(&manifest.shards[i].index))
                    .filter_map(|i| at[&i].map(|a| (i, a)))
                    .min_by(|x, y| {
                        ratio(&budgets, x.1)
                            .total_cmp(&ratio(&budgets, y.1))
                            .then(x.0.cmp(&y.0))
                    });
                let Some((i, from)) = source else { break };
                let size = manifest.shards[i].size;
                let key = self.accounts[from].key.clone();
                let Some(to) = best(&budgets, &counts, size, Some(&key)) else {
                    break;
                };
                let (src, dst) = (&self.accounts[from], &self.accounts[to]);
                let (bs, bd) = (budgets[&src.capacity_domain], budgets[&dst.capacity_domain]);
                let gap = ratio(&budgets, to) - ratio(&budgets, from);
                let after =
                    (bd - size) as f64 / dst.total as f64 >= (bs + size) as f64 / src.total as f64;
                if gap <= MIN_GAP || !after || src.capacity_domain == dst.capacity_domain {
                    break;
                }
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
                "rebalance: {capped} shard(s) over the {} limit per account",
                self.placement.cli_value()
            ));
        }
        if balanced > 0 {
            reasons.push(format!(
                "rebalance: {balanced} shard(s) moved to accounts with more free space"
            ));
        }
        (moves, reasons)
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
