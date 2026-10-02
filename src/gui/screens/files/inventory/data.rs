//! Rows of the archive inventory table: each inventory entry with the saved
//! pool it belongs to (matched by remotes and coding).
use crate::gui::i18n::tr;
use crate::inventory::load_inventory;
use crate::models::{InventoryEntry, PoolDefinition};
use crate::pool::load_pool_store;
use crate::remote_root::apply_remote_roots;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

/// Pool marker for an archive whose remotes match several pools.
const MULTIPLE_POOLS: &str = "Multiple";

#[derive(Debug, Clone)]
/// One inventory entry plus its matched pool.
pub(crate) struct InventoryRow {
    /// The inventory record.
    pub(crate) entry: InventoryEntry,
    /// Matching pool name, `MULTIPLE_POOLS` when several match, `None` when none.
    pub(crate) pool: Option<String>,
}

impl InventoryRow {
    /// Pool column text: name, "Multiple" or a dash.
    pub(crate) fn pool_label(&self) -> &str {
        match self.pool.as_deref() {
            None => "—",
            Some(MULTIPLE_POOLS) => tr("Multiple"),
            Some(pool) => pool,
        }
    }

    /// Coding column text (`data+parity`, or "No EC").
    pub(crate) fn coding_label(&self) -> String {
        self.entry
            .coding
            .as_ref()
            .map(|coding| format!("{}+{}", coding.data_shards, coding.parity_shards))
            .unwrap_or_else(|| tr("No EC").to_string())
    }
}

/// Load the inventory and match each entry to a saved pool; a missing pool
/// store just leaves pools unmatched. Called by the inventory state loader.
pub(crate) fn load_rows() -> Result<Vec<InventoryRow>> {
    let inventory = load_inventory()?;
    let pools = load_pool_store()
        .map(|store| store.pools)
        .unwrap_or_default();

    Ok(inventory
        .entries
        .into_values()
        .map(|entry| InventoryRow {
            pool: match_pool(&entry, &pools),
            entry,
        })
        .collect())
}

/// The pool whose resolved remotes and coding equal the entry's.
fn match_pool(entry: &InventoryEntry, pools: &BTreeMap<String, PoolDefinition>) -> Option<String> {
    let entry_remotes = normalized_set(entry.remotes.iter().cloned());
    let mut matches = Vec::new();

    for (name, pool) in pools {
        if !coding_matches(entry, pool) {
            continue;
        }
        let resolved =
            apply_remote_roots(pool.remotes.clone()).unwrap_or_else(|_| pool.remotes.clone());
        if normalized_set(resolved) == entry_remotes {
            matches.push(name.clone());
        }
    }

    match matches.len() {
        0 => None,
        1 => matches.into_iter().next(),
        _ => Some(MULTIPLE_POOLS.to_string()),
    }
}

/// Same data/parity counts; an entry without coding matches a pool without parity.
fn coding_matches(entry: &InventoryEntry, pool: &PoolDefinition) -> bool {
    match &entry.coding {
        Some(coding) => {
            coding.data_shards == pool.data_shards && coding.parity_shards == pool.parity_shards
        }
        None => pool.parity_shards == 0,
    }
}

/// Trimmed remote strings without trailing `/`, as a set.
fn normalized_set(values: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .collect()
}
