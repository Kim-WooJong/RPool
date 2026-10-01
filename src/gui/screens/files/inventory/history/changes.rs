//! Starting history changes in the task runner (they show in Jobs) and
//! taking their results when the task finishes.

use super::parse;
use super::state::{Change, HistoryForm};
use crate::gui::i18n::{tr, trf};
use crate::gui::task::{JobStatus, LogKind, TaskRunner};
use std::ffi::OsString;

/// Starts `change` for `pool`; a refusal (another task running) becomes the
/// pool's notice.
pub(crate) fn start(
    form: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
    pool: &str,
    change: Change,
    args: Vec<OsString>,
) {
    let result = task.start_rpool(change.task_name(), rclone, args);
    let state = form.pool(pool);
    let slot = match change {
        Change::Retention(_) => &mut state.retention_notice,
        Change::Cleanup { .. } => &mut state.cleanup_notice,
        _ => &mut state.notice,
    };
    match result {
        Ok(()) => {
            *slot = None;
            form.running = Some((pool.to_string(), change));
        }
        Err(error) => *slot = Some((false, error)),
    }
}

/// The notice for a finished change.
pub(crate) fn success_text(change: &Change) -> String {
    match change {
        Change::Restore { count: 1 } => tr("Restored 1 item from the trash.").into(),
        Change::Restore { count } => trf("Restored {n} items from the trash.", &[("n", count)]),
        Change::Purge { count: 1 } => tr("Deleted 1 item permanently.").into(),
        Change::Purge { count } => trf("Deleted {n} items permanently.", &[("n", count)]),
        Change::PurgeExpired => tr("Deleted the expired items permanently.").into(),
        Change::Empty => tr("The trash is empty now.").into(),
        Change::Version {
            name,
            as_copy: false,
        } => trf(
            "Restored an earlier version of {name}; the replaced one is kept as a version.",
            &[("name", name)],
        ),
        Change::Version {
            name,
            as_copy: true,
        } => trf(
            "Restored an earlier version of {name} as a copy next to it.",
            &[("name", name)],
        ),
        Change::Rollback { changes, .. } => trf(
            "Rolled back {n} changes. Files created after that time are in the trash.",
            &[("n", changes)],
        ),
        Change::Retention(_) => tr("Saved the trash and version settings.").into(),
        Change::Cleanup { .. } => tr("Cleanup finished. Newly found data waits for its grace period; data past it was deleted.").into(),
    }
}

/// After any task: finish a history change that was running.
pub(crate) fn handle_task_completion(form: &mut HistoryForm, task: &TaskRunner, status: JobStatus) {
    let Some((_, change)) = &form.running else {
        return;
    };
    if task
        .last_task()
        .is_none_or(|last| last.name != change.task_name())
    {
        return;
    }
    let Some((pool, change)) = form.running.take() else {
        return;
    };
    let notice = if status == JobStatus::Completed {
        match &change {
            Change::Rollback { trashed, .. } => {
                form.pool(&pool)
                    .from_rollback
                    .extend(trashed.iter().cloned());
                form.rollback = None;
            }
            Change::Retention(saved) => {
                let history = form.pool(&pool);
                history.retention = super::query::Fetch::ready(*saved);
                history.retention_draft = None;
            }
            Change::Restore { .. } | Change::Purge { .. } | Change::Empty => {
                form.pool(&pool).selection.clear();
            }
            _ => {}
        }
        (true, success_text(&change))
    } else {
        if let Some(dialog) = form.rollback.as_mut() {
            dialog.confirming = false;
        }
        let stderr: Vec<&str> = task
            .logs()
            .iter()
            .filter(|line| line.kind == LogKind::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        let error = parse::last_error(&stderr.join("\n"));
        let text = match (status, error) {
            (JobStatus::Cancelled, _) => tr("The change was cancelled.").into(),
            (_, Some(error)) => trf(
                "The change failed: {error} (see Jobs for details).",
                &[("error", &error)],
            ),
            (_, None) => tr("The change failed; see Jobs for details.").into(),
        };
        (false, text)
    };
    // Retention changes touch no files; everything else reloads the views.
    if matches!(change, Change::Retention(_)) {
        form.pool(&pool).retention_notice = Some(notice);
    } else if matches!(change, Change::Cleanup { .. }) {
        let history = form.pool(&pool);
        history.cleanup_confirm = super::state::CleanupConfirm::None;
        // Show the new state; deleted versions are no longer restorable.
        history.cleanup.stale = history.cleanup.value.is_some();
        history.trash.stale = true;
        history.cleanup_notice = Some(notice);
        if let Some(panel) = form.versions.as_mut().filter(|v| v.pool == pool) {
            panel.fetch.stale = true;
        }
    } else {
        form.invalidate(&pool);
        form.pool(&pool).notice = Some(notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::screens::files::inventory::history::query::Fetch;

    #[test]
    fn completion_updates_the_pool() {
        let mut form = HistoryForm::default();
        let mut task = TaskRunner::default();
        let change = Change::Rollback {
            changes: 7,
            trashed: vec!["/Docs/new.txt".into()],
        };
        task.fake_running(change.task_name());
        form.running = Some(("family".into(), change));
        form.open_rollback("family", "/Docs".into(), String::new());
        form.pool("family").trash = Fetch::ready(Vec::new());
        task.fake_finish(true);
        let status = task.poll().unwrap();
        handle_task_completion(&mut form, &task, status);
        assert!(form.running.is_none() && form.rollback.is_none());
        let pool = form.pool("family");
        assert!(pool.from_rollback.contains("/Docs/new.txt"));
        assert!(pool.trash.needs_load(), "trash reloads");
        assert!(matches!(&pool.notice, Some((true, text)) if text.contains('7')));
        assert!(form.refresh_drive);

        // Another task's completion leaves a running change alone.
        let mut other = TaskRunner::default();
        other.fake_running("Speed test");
        form.running = Some(("family".into(), Change::Empty));
        other.fake_finish(true);
        let status = other.poll().unwrap();
        handle_task_completion(&mut form, &other, status);
        assert!(form.running.is_some());

        let mut failed = TaskRunner::default();
        failed.fake_running("Empty trash");
        failed.fake_finish(false);
        let status = failed.poll().unwrap();
        handle_task_completion(&mut form, &failed, status);
        assert!(matches!(&form.pool("family").notice, Some((false, _))));
    }

    #[test]
    fn a_finished_cleanup_refreshes_its_preview() {
        let mut form = HistoryForm::default();
        let mut task = TaskRunner::default();
        let change = Change::Cleanup { force: true };
        task.fake_running(change.task_name());
        form.running = Some(("p".into(), change));
        let pool = form.pool("p");
        pool.cleanup = Fetch::ready(super::super::sample::cleanup_report(1_000));
        pool.cleanup_confirm = super::super::state::CleanupConfirm::Guard;
        task.fake_finish(true);
        let status = task.poll().unwrap();
        handle_task_completion(&mut form, &task, status);
        let pool = form.pool("p");
        assert!(pool.cleanup.needs_load() && pool.trash.needs_load());
        assert_eq!(
            pool.cleanup_confirm,
            super::super::state::CleanupConfirm::None
        );
        assert!(matches!(&pool.cleanup_notice, Some((true, _))));
        assert!(pool.notice.is_none());
    }

    #[test]
    fn saved_retention_replaces_the_shown_values() {
        let mut form = HistoryForm::default();
        let mut task = TaskRunner::default();
        let saved = super::super::sample::retention();
        let change = Change::Retention(saved);
        task.fake_running(change.task_name());
        form.running = Some(("p".into(), change));
        form.pool("p").retention_draft = Some(saved);
        task.fake_finish(true);
        let status = task.poll().unwrap();
        handle_task_completion(&mut form, &task, status);
        assert_eq!(form.pool("p").retention.ok(), Some(&saved));
        assert!(form.pool("p").retention_draft.is_none());
        assert_eq!(
            success_text(&Change::Restore { count: 1 }),
            "Restored 1 item from the trash."
        );
    }
}
