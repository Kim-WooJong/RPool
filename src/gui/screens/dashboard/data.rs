use crate::history::load_history;
use crate::inventory::load_inventory;
use crate::models::TaskRecord;
use crate::pool::load_pool_store;

#[derive(Debug, Clone)]
pub(crate) struct DashboardPool {
    pub(crate) name: String,
    pub(crate) remotes: Vec<String>,
    pub(crate) data_shards: usize,
    pub(crate) parity_shards: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DashboardData {
    pub(crate) file_count: usize,
    pub(crate) logical_bytes: u64,
    pub(crate) pools: Vec<DashboardPool>,
    pub(crate) recent_jobs: Vec<TaskRecord>,
    pub(crate) warnings: Vec<String>,
}

impl DashboardData {
    pub(crate) fn load() -> Self {
        let mut data = Self::default();

        match load_inventory() {
            Ok(store) => {
                data.file_count = store.entries.len();
                data.logical_bytes = store
                    .entries
                    .values()
                    .fold(0_u64, |total, entry| total.saturating_add(entry.original_size));
            }
            Err(error) => data
                .warnings
                .push(format!("Inventory could not be loaded: {error:#}")),
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
            Err(error) => data
                .warnings
                .push(format!("Pool configuration could not be loaded: {error:#}")),
        }

        match load_history() {
            Ok(mut records) => {
                records.sort_by_key(|record| std::cmp::Reverse(record.finished_unix));
                records.truncate(5);
                data.recent_jobs = records;
            }
            Err(error) => data
                .warnings
                .push(format!("Task history could not be loaded: {error:#}")),
        }

        data
    }

    pub(crate) fn refresh(&mut self) {
        *self = Self::load();
    }
}
