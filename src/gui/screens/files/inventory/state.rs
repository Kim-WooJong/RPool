use super::data::{load_rows, InventoryRow};
use std::cmp::Ordering;
use std::collections::BTreeSet;

pub(crate) const UNMATCHED_POOL: &str = "Unmatched";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InventorySort {
    Name,
    Size,
    Pool,
    Coding,
    Created,
}

impl Default for InventorySort {
    fn default() -> Self {
        Self::Name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodingFilter {
    All,
    ReedSolomon,
    Plain,
}

impl Default for CodingFilter {
    fn default() -> Self {
        Self::All
    }
}

impl CodingFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All coding",
            Self::ReedSolomon => "Reed-Solomon",
            Self::Plain => "No erasure coding",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct InventoryForm {
    pub(crate) manifest_directory: String,
    pub(crate) error: Option<String>,
    pub(crate) query: String,
    pub(crate) pool_filter: String,
    pub(crate) coding_filter: CodingFilter,
    pub(crate) sort: InventorySort,
    pub(crate) descending: bool,
    pub(crate) selected_archive_id: Option<String>,
    pub(crate) show_rebuild: bool,
    pub(crate) loaded: bool,
    pub(crate) rows: Vec<InventoryRow>,
}

impl InventoryForm {
    pub(crate) fn refresh(&mut self) {
        match load_rows() {
            Ok(rows) => {
                self.rows = rows;
                self.error = None;
                self.loaded = true;
                if self
                    .selected_archive_id
                    .as_ref()
                    .is_some_and(|id| !self.rows.iter().any(|row| &row.entry.archive_id == id))
                {
                    self.selected_archive_id = None;
                }
            }
            Err(error) => {
                self.error = Some(format!("Inventory could not be loaded: {error:#}"));
                self.rows.clear();
                self.loaded = true;
                self.selected_archive_id = None;
            }
        }
    }

    pub(crate) fn visible_rows(&self) -> Vec<InventoryRow> {
        let query = self.query.trim().to_ascii_lowercase();
        let mut rows: Vec<_> = self
            .rows
            .iter()
            .filter(|row| self.matches_query(row, &query))
            .filter(|row| self.matches_pool(row))
            .filter(|row| self.matches_coding(row))
            .cloned()
            .collect();

        rows.sort_by(|left, right| self.compare_rows(left, right));
        if self.descending {
            rows.reverse();
        }
        rows
    }

    pub(crate) fn pool_options(&self) -> Vec<String> {
        let mut pools = BTreeSet::new();
        let mut has_unmatched = false;
        for row in &self.rows {
            if let Some(pool) = &row.pool {
                pools.insert(pool.clone());
            } else {
                has_unmatched = true;
            }
        }
        let mut values: Vec<_> = pools.into_iter().collect();
        if has_unmatched {
            values.push(UNMATCHED_POOL.to_string());
        }
        values
    }

    pub(crate) fn set_sort(&mut self, sort: InventorySort) {
        if self.sort == sort {
            self.descending = !self.descending;
        } else {
            self.sort = sort;
            self.descending = false;
        }
    }

    pub(crate) fn sort_label(&self, sort: InventorySort, label: &str) -> String {
        if self.sort != sort {
            return label.to_string();
        }
        format!("{label} {}", if self.descending { "↓" } else { "↑" })
    }

    fn matches_query(&self, row: &InventoryRow, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        row.entry.original_name.to_ascii_lowercase().contains(query)
            || row.entry.archive_id.to_ascii_lowercase().contains(query)
            || row.entry.manifest_source.to_ascii_lowercase().contains(query)
            || row.pool_label().to_ascii_lowercase().contains(query)
            || row
                .entry
                .remotes
                .iter()
                .any(|remote| remote.to_ascii_lowercase().contains(query))
    }

    fn matches_pool(&self, row: &InventoryRow) -> bool {
        if self.pool_filter.is_empty() {
            return true;
        }
        if self.pool_filter == UNMATCHED_POOL {
            return row.pool.is_none();
        }
        row.pool.as_deref() == Some(self.pool_filter.as_str())
    }

    fn matches_coding(&self, row: &InventoryRow) -> bool {
        match self.coding_filter {
            CodingFilter::All => true,
            CodingFilter::ReedSolomon => row.entry.coding.is_some(),
            CodingFilter::Plain => row.entry.coding.is_none(),
        }
    }

    fn compare_rows(&self, left: &InventoryRow, right: &InventoryRow) -> Ordering {
        match self.sort {
            InventorySort::Name => left
                .entry
                .original_name
                .to_ascii_lowercase()
                .cmp(&right.entry.original_name.to_ascii_lowercase()),
            InventorySort::Size => left.entry.original_size.cmp(&right.entry.original_size),
            InventorySort::Pool => left.pool_label().cmp(right.pool_label()),
            InventorySort::Coding => left.coding_label().cmp(&right.coding_label()),
            InventorySort::Created => left.entry.created_unix.cmp(&right.entry.created_unix),
        }
        .then_with(|| left.entry.archive_id.cmp(&right.entry.archive_id))
    }
}
