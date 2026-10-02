//! A rollback plan grouped for the preview: files that get an older
//! version back, deleted files that come back, new files that move to the
//! trash, and paths that cannot be rolled back.

use crate::drive_history::model::{ChangeAction, RollbackChange, RollbackPlan};

/// Changes of one kind in a rollback preview, with their total size.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Group {
    /// Sorted by path.
    pub(crate) changes: Vec<RollbackChange>,
    /// Sum of the changes' sizes in bytes (saturating).
    pub(crate) bytes: u64,
}

impl Group {
    /// Adds one change and its size.
    fn push(&mut self, change: &RollbackChange) {
        self.bytes = self.bytes.saturating_add(change.size);
        self.changes.push(change.clone());
    }
}

/// A `RollbackPlan` split into the preview groups; built by
/// `RollbackSummary::of` in the rollback dialog.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RollbackSummary {
    /// Files that get an older version back (`ChangeAction::Revert`).
    pub(crate) reverted: Group,
    /// Deleted files that come back (`ChangeAction::Undelete`).
    pub(crate) restored: Group,
    /// Files created after the time, moving to the trash (`ChangeAction::Remove`).
    pub(crate) trashed: Group,
    /// `(path, reason)`, sorted by path.
    pub(crate) skipped: Vec<(String, String)>,
}

impl RollbackSummary {
    /// Groups and sorts the plan's changes and copies its skipped paths.
    pub(crate) fn of(plan: &RollbackPlan) -> Self {
        let mut summary = Self::default();
        for change in &plan.changes {
            match change.action {
                ChangeAction::Revert => summary.reverted.push(change),
                ChangeAction::Undelete => summary.restored.push(change),
                ChangeAction::Remove => summary.trashed.push(change),
            }
        }
        for group in [
            &mut summary.reverted,
            &mut summary.restored,
            &mut summary.trashed,
        ] {
            group.changes.sort_by(|a, b| a.path.cmp(&b.path));
        }
        summary.skipped = plan.skipped.clone();
        summary.skipped.sort();
        summary
    }

    /// Changes the rollback would make.
    pub(crate) fn changes(&self) -> usize {
        self.reverted.changes.len() + self.restored.changes.len() + self.trashed.changes.len()
    }

    /// Paths that go to the trash, to mark them in the trash afterwards.
    pub(crate) fn trashed_paths(&self) -> Vec<String> {
        self.trashed
            .changes
            .iter()
            .map(|change| change.path.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_sorts_and_counts() {
        let plan = super::super::sample::rollback_plan("/Docs", 1_000);
        let summary = RollbackSummary::of(&plan);
        assert_eq!(summary.changes(), plan.changes.len());
        assert_eq!(summary.reverted.changes.len(), 3);
        assert_eq!(summary.restored.changes.len(), 2);
        assert_eq!(summary.trashed.changes.len(), 2);
        assert_eq!(summary.skipped.len(), 1);
        let paths: Vec<_> = summary
            .reverted
            .changes
            .iter()
            .map(|c| c.path.as_str())
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        assert_eq!(
            summary.reverted.bytes,
            summary.reverted.changes.iter().map(|c| c.size).sum::<u64>()
        );
        assert_eq!(summary.trashed_paths().len(), 2);
        assert!(summary
            .trashed_paths()
            .iter()
            .all(|p| p.starts_with("/Docs/")));
        let empty = RollbackSummary::of(&RollbackPlan {
            changes: Vec::new(),
            skipped: Vec::new(),
            ..plan
        });
        assert_eq!(empty.changes(), 0);
    }
}
