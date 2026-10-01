//! Trash entries as list rows: name and original folder, texts for the
//! deletion time and expiry, filtered by the search and newest first.

use super::format::{expires_soon, expiry, is_expired, when};
use super::paths::split;
use crate::drive_history::model::TrashEntry;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrashRow {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) name: String,
    /// Original folder (drive path, `/` for the root).
    pub(crate) folder: String,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    /// "3h ago" and the local time for the tooltip.
    pub(crate) deleted: (String, String),
    pub(crate) deleted_by: Option<String>,
    pub(crate) expiry: String,
    pub(crate) expired: bool,
    pub(crate) expires_soon: bool,
    pub(crate) path_taken: bool,
    /// Moved here by a rollback applied in this session.
    pub(crate) from_rollback: bool,
    deleted_unix: Option<u64>,
}

pub(crate) fn rows(
    entries: &[TrashEntry],
    query: &str,
    now: u64,
    offset: i64,
    from_rollback: &BTreeSet<String>,
) -> Vec<TrashRow> {
    let query = query.trim().to_lowercase();
    let mut rows: Vec<TrashRow> = entries
        .iter()
        .filter(|entry| query.is_empty() || entry.path.to_lowercase().contains(&query))
        .map(|entry| {
            let (folder, name) = split(&entry.path);
            TrashRow {
                id: entry.id.clone(),
                path: entry.path.clone(),
                name,
                folder,
                is_dir: entry.is_dir,
                size: entry.size,
                deleted: when(now, entry.deleted_unix, offset),
                deleted_by: entry.deleted_by.clone(),
                expiry: expiry(now, entry.expires_unix),
                expired: is_expired(now, entry.expires_unix),
                expires_soon: expires_soon(now, entry.expires_unix),
                path_taken: entry.path_taken,
                from_rollback: from_rollback.contains(&entry.path),
                deleted_unix: entry.deleted_unix,
            }
        })
        .collect();
    // Newest deletion first; unknown times last; then by path.
    rows.sort_by(|a, b| {
        b.deleted_unix
            .cmp(&a.deleted_unix)
            .then_with(|| a.path.cmp(&b.path))
    });
    rows
}

/// `(count, bytes)` of the rows whose id is in `ids`.
pub(crate) fn totals(rows: &[TrashRow], ids: &[String]) -> (usize, u64) {
    rows.iter()
        .filter(|row| ids.contains(&row.id))
        .fold((0, 0), |(n, bytes), row| {
            (n + 1, bytes.saturating_add(row.size))
        })
}

/// `(count, bytes)` of every entry.
pub(crate) fn all_totals(entries: &[TrashEntry]) -> (usize, u64) {
    entries.iter().fold((0, 0), |(n, bytes), entry| {
        (n + 1, bytes.saturating_add(entry.size))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_from_sample_entries() {
        let now = 1_727_740_800;
        let entries = super::super::sample::trash(now);
        let marked = BTreeSet::from(["/Docs/new-draft.txt".to_string()]);
        let all = rows(&entries, "", now, 0, &marked);
        assert_eq!(all.len(), entries.len());
        let times: Vec<_> = all.iter().map(|r| r.deleted_unix).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted, "newest first");
        let report = all.iter().find(|r| r.name == "report.pdf").unwrap();
        assert_eq!(report.folder, "/Documents");
        assert!(report.path_taken);
        assert!(all.iter().any(|r| r.from_rollback));
        assert!(all.iter().any(|r| r.expired));
        assert!(all.iter().any(|r| r.is_dir));
        let found = rows(&entries, "  REPORT ", now, 0, &marked);
        assert!(
            !found.is_empty()
                && found
                    .iter()
                    .all(|r| r.path.to_lowercase().contains("report"))
        );
        let ids: Vec<String> = found.iter().map(|r| r.id.clone()).collect();
        let (n, bytes) = totals(&all, &ids);
        assert_eq!(n, found.len());
        assert_eq!(bytes, found.iter().map(|r| r.size).sum::<u64>());
        assert_eq!(all_totals(&entries).0, entries.len());
    }
}
