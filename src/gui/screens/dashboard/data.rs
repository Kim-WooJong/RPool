//! Data behind the overview page: inventory totals, saved pools, the newest
//! task records and load warnings, read from local files by `DashboardData::load`.
use crate::gui::i18n::trf;
use crate::history::load_history;
use crate::inventory::load_inventory;
use crate::models::TaskRecord;
use crate::pool::load_pool_store;

#[derive(Debug, Clone)]
/// One saved pool as the overview shows it.
pub(crate) struct DashboardPool {
    /// Pool name.
    pub(crate) name: String,
    /// Provider remotes of the pool.
    pub(crate) remotes: Vec<String>,
    /// Reed-Solomon data shards.
    pub(crate) data_shards: usize,
    /// Reed-Solomon parity shards (0 = no parity).
    pub(crate) parity_shards: usize,
}

#[derive(Debug, Clone, Default)]
/// Everything the overview page shows from local state; kept in
/// `GuiState::dashboard` and refreshed after tasks finish.
pub(crate) struct DashboardData {
    /// Archives in the inventory.
    pub(crate) file_count: usize,
    /// Sum of the archives' original sizes in bytes.
    pub(crate) logical_bytes: u64,
    /// Saved pools.
    pub(crate) pools: Vec<DashboardPool>,
    /// Up to five newest task history records.
    pub(crate) recent_jobs: Vec<TaskRecord>,
    /// Translated messages for files that could not be loaded.
    pub(crate) warnings: Vec<String>,
}

impl DashboardData {
    /// Read inventory, pool store and task history; failures become warnings.
    pub(crate) fn load() -> Self {
        let mut data = Self::default();

        match load_inventory() {
            Ok(store) => {
                data.file_count = store.entries.len();
                data.logical_bytes = store.entries.values().fold(0_u64, |total, entry| {
                    total.saturating_add(entry.original_size)
                });
            }
            Err(error) => data.warnings.push(trf(
                "Inventory could not be loaded: {error}",
                &[("error", &format!("{error:#}"))],
            )),
        }

        match load_pool_store() {
            Ok(store) => {
                data.pools = store
                    .pools
                    .into_iter()
                    .map(|(name, pool)| DashboardPool {
                        name,
                        remotes: pool.remotes,
                        data_shards: pool.data_shards,
                        parity_shards: pool.parity_shards,
                    })
                    .collect();
            }
            Err(error) => data.warnings.push(trf(
                "Pool configuration could not be loaded: {error}",
                &[("error", &format!("{error:#}"))],
            )),
        }

        match load_history() {
            Ok(mut records) => {
                records.sort_by_key(|record| std::cmp::Reverse(record.finished_unix));
                records.truncate(5);
                data.recent_jobs = records;
            }
            Err(error) => data.warnings.push(trf(
                "Task history could not be loaded: {error}",
                &[("error", &format!("{error:#}"))],
            )),
        }

        data
    }

    /// Reload everything from disk.
    pub(crate) fn refresh(&mut self) {
        *self = Self::load();
    }
}
