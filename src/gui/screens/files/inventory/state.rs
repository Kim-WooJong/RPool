//! State of Files › Library (`InventoryForm`): the drive browser, and the
//! uploaded-archives list with its search, filters, sort and selection.

use super::data::{load_rows, InventoryRow};
use super::drive_state::DriveForm;
use crate::gui::i18n::{tr, trf};
use std::cmp::Ordering;

/// Pool filter value selecting archives whose remotes matched no pool.
pub(crate) const UNMATCHED_POOL: &str = "Unmatched";

/// Column the archives table is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InventorySort {
    /// Original file name, case-insensitive.
    #[default]
    Name,
    /// Original size in bytes.
    Size,
    /// Matched pool label.
    Pool,
    /// Coding label (e.g. "9+3", or no EC).
    Coding,
    /// Upload time.
    Created,
}

/// Coding filter of the archives table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CodingFilter {
    /// Every archive.
    #[default]
    All,
    /// Only Reed-Solomon coded archives.
    ReedSolomon,
    /// Only archives stored without erasure coding.
    Plain,
}

impl CodingFilter {
    /// Label shown in the coding filter combo box.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => tr("All coding"),
            Self::ReedSolomon => "Reed-Solomon",
            Self::Plain => tr("No erasure coding"),
        }
    }
}

/// Library sub-view: the selected pool's drive, or its uploaded archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LibraryView {
    /// Read-only explorer of the pool's drive.
    #[default]
    Drive,
    /// Archives from the local inventory.
    Archives,
}

/// Whole state of Files › Library, held in `GuiState::inventory`.
#[derive(Debug, Default)]
pub(crate) struct InventoryForm {
    /// Selected tab (drive or archives).
    pub(crate) view: LibraryView,
    /// Pool drive browser; its `pool` is also the archives' pool filter.
    pub(crate) drive: DriveForm,
    /// Folder typed or picked in the Rebuild inventory panel.
    pub(crate) manifest_directory: String,
    /// Last load or rebuild-start error, shown above the table.
    pub(crate) error: Option<String>,
    /// Search text of the archives table.
    pub(crate) query: String,
    /// Pool whose archives are listed (`""` = all, `UNMATCHED_POOL` = none matched);
    /// set from the drive's selected pool each frame.
    pub(crate) pool_filter: String,
    /// Coding filter of the archives table.
    pub(crate) coding_filter: CodingFilter,
    /// Sort column of the archives table.
    pub(crate) sort: InventorySort,
    /// Reverse the sort order.
    pub(crate) descending: bool,
    /// Archive ID of the selected row, if any.
    pub(crate) selected_archive_id: Option<String>,
    /// Whether the Rebuild inventory panel is open.
    pub(crate) show_rebuild: bool,
    /// Whether `rows` has been loaded at least once (also after a failed load).
    pub(crate) loaded: bool,
    /// All archives from the local inventory, with their matched pool.
    pub(crate) rows: Vec<InventoryRow>,
}

impl InventoryForm {
    /// Reloads `rows` from the local inventory; drops a selection that no longer
    /// exists. On error clears the rows and stores a message in `error`.
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
                self.error = Some(trf(
                    "Inventory could not be loaded: {error}",
                    &[("error", &format!("{error:#}"))],
                ));
                self.rows.clear();
                self.loaded = true;
                self.selected_archive_id = None;
            }
        }
    }

    /// Rows matching the search, pool and coding filters, sorted by the current
    /// column and direction. Called by `inventory::archives` every frame.
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

    /// Archives outside the pool filter (other pools, or none matched).
    pub(crate) fn outside_pool(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| !self.matches_pool(row))
            .count()
    }

    /// Header click: flips the direction on the active column, otherwise sorts
    /// ascending by `sort`.
    pub(crate) fn set_sort(&mut self, sort: InventorySort) {
        if self.sort == sort {
            self.descending = !self.descending;
        } else {
            self.sort = sort;
            self.descending = false;
        }
    }

    /// Header text with an ↑/↓ arrow on the active column.
    pub(crate) fn sort_label(&self, sort: InventorySort, label: &str) -> String {
        if self.sort != sort {
            return label.to_string();
        }
        format!("{label} {}", if self.descending { "↓" } else { "↑" })
    }

    /// Case-insensitive substring match of `query` (already lowercased) on name,
    /// archive ID, manifest path, pool label or any remote.
    fn matches_query(&self, row: &InventoryRow, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        row.entry.original_name.to_ascii_lowercase().contains(query)
            || row.entry.archive_id.to_ascii_lowercase().contains(query)
            || row
                .entry
                .manifest_source
                .to_ascii_lowercase()
                .contains(query)
            || row.pool_label().to_ascii_lowercase().contains(query)
            || row
                .entry
                .remotes
                .iter()
                .any(|remote| remote.to_ascii_lowercase().contains(query))
    }

    /// Whether the row belongs to `pool_filter`.
    fn matches_pool(&self, row: &InventoryRow) -> bool {
        if self.pool_filter.is_empty() {
            return true;
        }
        if self.pool_filter == UNMATCHED_POOL {
            return row.pool.is_none();
        }
        row.pool.as_deref() == Some(self.pool_filter.as_str())
    }

    /// Whether the row passes `coding_filter`.
    fn matches_coding(&self, row: &InventoryRow) -> bool {
        match self.coding_filter {
            CodingFilter::All => true,
            CodingFilter::ReedSolomon => row.entry.coding.is_some(),
            CodingFilter::Plain => row.entry.coding.is_none(),
        }
    }

    /// Order by the sort column, ties broken by archive ID (ascending only;
    /// `visible_rows` reverses for descending).
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
