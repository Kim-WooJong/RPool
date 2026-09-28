use crate::inventory::load_inventory;
use crate::models::{InventoryEntry, PoolDefinition};
use crate::pool::load_pool_store;
use crate::remote_root::apply_remote_roots;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub(crate) struct InventoryRow {
    pub(crate) entry: InventoryEntry,
    pub(crate) pool: Option<String>,
}

impl InventoryRow {
    pub(crate) fn pool_label(&self) -> &str {
        self.pool.as_deref().unwrap_or("—")
    }

    pub(crate) fn coding_label(&self) -> String {
        self.entry
            .coding
            .as_ref()
            .map(|coding| format!("{}+{}", coding.data_shards, coding.parity_shards))
            .unwrap_or_else(|| "No EC".to_string())
    }
}

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
        _ => Some("Multiple".to_string()),
    }
}

fn coding_matches(entry: &InventoryEntry, pool: &PoolDefinition) -> bool {
    match &entry.coding {
        Some(coding) => {
            coding.data_shards == pool.data_shards && coding.parity_shards == pool.parity_shards
        }
        None => pool.parity_shards == 0,
    }
}

fn normalized_set(values: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .collect()
}
