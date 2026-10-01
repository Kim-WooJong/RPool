//! In-memory trash / versions / rollback state of the Library (per pool,
//! never saved) and the change that is running in the task runner.

use super::query::Fetch;
use super::rollback_time::TimePreset;
use super::selection::Selection;
use crate::drive_history::model::{Retention, RollbackPlan, TrashEntry, VersionEntry};
use std::collections::{BTreeMap, BTreeSet};

/// Trash, retention and notices of one pool.
#[derive(Debug, Default)]
pub(crate) struct PoolHistory {
    pub(crate) trash: Fetch<Vec<TrashEntry>>,
    pub(crate) selection: Selection,
    /// Drive paths a rollback applied in this session moved to the trash.
    pub(crate) from_rollback: BTreeSet<String>,
    pub(crate) retention: Fetch<Retention>,
    /// Values being edited on the Pools card (`None`: show the saved ones).
    pub(crate) retention_draft: Option<Retention>,
    /// Result of the last change: `(success, text)`.
    pub(crate) notice: Option<(bool, String)>,
    /// Result of the last retention change (shown on the Pools card).
    pub(crate) retention_notice: Option<(bool, String)>,
}

/// The versions side panel of one file.
#[derive(Debug, Default)]
pub(crate) struct VersionsPanel {
    pub(crate) pool: String,
    /// Drive path (`/Docs/a.txt`).
    pub(crate) path: String,
    pub(crate) fetch: Fetch<Vec<VersionEntry>>,
    pub(crate) selected: Option<String>,
}

/// The "Roll back…" dialog.
#[derive(Debug, Default)]
pub(crate) struct RollbackDialog {
    pub(crate) pool: String,
    /// Folder (drive path, `/` for the whole drive).
    pub(crate) scope: String,
    pub(crate) preset: TimePreset,
    /// Typed local time for [`TimePreset::Custom`].
    pub(crate) custom: String,
    pub(crate) preview: Fetch<RollbackPlan>,
    /// Second step: "Apply rollback" was clicked once.
    pub(crate) confirming: bool,
}

/// Confirmation dialogs of the trash view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrashDialog {
    Purge {
        ids: Vec<String>,
        count: usize,
        bytes: u64,
    },
    Empty {
        count: usize,
        bytes: u64,
    },
    /// "Restore to…": the chosen folder (explorer path, `""` = root).
    RestoreTo {
        ids: Vec<String>,
        folder: String,
        filter: String,
    },
}

/// A change started in the task runner, to finish when it completes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Change {
    Restore {
        count: usize,
    },
    Purge {
        count: usize,
    },
    PurgeExpired,
    Empty,
    Version {
        name: String,
        as_copy: bool,
    },
    Rollback {
        changes: usize,
        trashed: Vec<String>,
    },
    Retention(Retention),
}

impl Change {
    /// Task name in Jobs (not translated, like other task names).
    pub(crate) fn task_name(&self) -> &'static str {
        match self {
            Change::Restore { .. } => "Trash restore",
            Change::Purge { .. } | Change::PurgeExpired => "Trash purge",
            Change::Empty => "Empty trash",
            Change::Version { .. } => "Version restore",
            Change::Rollback { .. } => "Rollback",
            Change::Retention(_) => "Trash and version settings",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HistoryForm {
    /// The Library shows the trash instead of the drive.
    pub(crate) trash_open: bool,
    pools: BTreeMap<String, PoolHistory>,
    pub(crate) versions: Option<VersionsPanel>,
    pub(crate) rollback: Option<RollbackDialog>,
    pub(crate) dialog: Option<TrashDialog>,
    /// `(pool, change)` running in the task runner.
    pub(crate) running: Option<(String, Change)>,
    /// The drive listing should be read again (after a change).
    pub(crate) refresh_drive: bool,
    /// Scroll the Pools page to the "Trash & versions" card next frame.
    pub(crate) reveal_retention: bool,
}

impl HistoryForm {
    pub(crate) fn pool(&mut self, pool: &str) -> &mut PoolHistory {
        self.pools.entry(pool.to_string()).or_default()
    }

    pub(crate) fn get(&self, pool: &str) -> Option<&PoolHistory> {
        self.pools.get(pool)
    }

    /// Forgets removed pools and closes panels of another pool.
    pub(crate) fn sync(&mut self, pools: &[String], current: &str) {
        self.pools.retain(|pool, _| pools.contains(pool));
        if self.versions.as_ref().is_some_and(|v| v.pool != current) {
            self.versions = None;
        }
        if self.rollback.as_ref().is_some_and(|r| r.pool != current) {
            self.rollback = None;
        }
        if self.running.is_none() && current.is_empty() {
            self.dialog = None;
        }
    }

    pub(crate) fn open_versions(&mut self, pool: &str, path: String) {
        if self
            .versions
            .as_ref()
            .is_some_and(|v| v.pool == pool && v.path == path)
        {
            return;
        }
        self.versions = Some(VersionsPanel {
            pool: pool.to_string(),
            path,
            ..Default::default()
        });
    }

    pub(crate) fn open_rollback(&mut self, pool: &str, scope: String, custom: String) {
        self.pool(pool).notice = None;
        self.rollback = Some(RollbackDialog {
            pool: pool.to_string(),
            scope,
            custom,
            ..Default::default()
        });
    }

    pub(crate) fn is_running(&self, pool: &str) -> bool {
        self.running.as_ref().is_some_and(|(p, _)| p == pool)
    }

    /// After a change: reload what it may have changed.
    pub(crate) fn invalidate(&mut self, pool: &str) {
        let history = self.pool(pool);
        history.trash.stale = true;
        history.retention.stale = history.retention.value.is_some();
        if let Some(panel) = self.versions.as_mut().filter(|v| v.pool == pool) {
            panel.fetch.stale = true;
        }
        self.refresh_drive = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools_panels_and_invalidation() {
        let mut form = HistoryForm::default();
        form.pool("a").trash = Fetch::ready(Vec::new());
        form.pool("b");
        form.open_versions("a", "/x".into());
        form.versions.as_mut().unwrap().fetch = Fetch::ready(Vec::new());
        form.open_versions("a", "/x".into());
        assert!(
            form.versions.as_ref().unwrap().fetch.ok().is_some(),
            "same file keeps its list"
        );
        form.open_rollback("a", "/".into(), String::new());
        form.invalidate("a");
        assert!(form.pool("a").trash.needs_load());
        assert!(form.versions.as_ref().unwrap().fetch.needs_load());
        assert!(form.refresh_drive);
        form.sync(&["a".to_string()], "a");
        assert!(form.get("b").is_none() && form.versions.is_some());
        form.sync(&["a".to_string()], "c");
        assert!(form.versions.is_none() && form.rollback.is_none());
        form.running = Some(("a".into(), Change::Empty));
        assert!(form.is_running("a") && !form.is_running("c"));
        assert_eq!(Change::PurgeExpired.task_name(), "Trash purge");
    }
}
