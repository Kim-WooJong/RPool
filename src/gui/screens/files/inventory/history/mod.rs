//! Trash, file versions and rollback in Library › Drive files, backed by
//! `rpool drive trash|versions|rollback|retention` (JSON contract in
//! `crate::drive_history::model`). Listings and previews run as read-only
//! queries; changes run in the task runner and show in Jobs.

pub(crate) mod args;
mod badges;
pub(crate) mod changes;
pub(crate) mod clock;
mod dialog;
mod format;
pub(crate) mod parse;
pub(crate) mod paths;
pub(crate) mod query;
pub(crate) mod retention;
mod rollback_dialog;
mod rollback_summary;
mod rollback_time;
#[cfg(any(test, debug_assertions))]
pub(crate) mod sample;
mod selection;
pub(crate) mod state;
mod trash_columns;
mod trash_dialogs;
mod trash_list;
mod trash_rows;
mod trash_view;
mod versions_panel;

pub(crate) use changes::handle_task_completion;
pub(crate) use state::HistoryForm;
pub(crate) use trash_view::show as trash;
pub(crate) use versions_panel::show as versions;

use super::drive_state::DriveTree;
use super::explorer::HistoryRequest;
use crate::gui::task::TaskRunner;
use eframe::egui;

/// Starts the listings the open views need and collects finished ones.
/// Returns true while any is running (to keep repainting).
pub(crate) fn poll(form: &mut HistoryForm, pool: &str, rclone: &str) -> bool {
    let mut busy = false;
    if !pool.is_empty() {
        let trash_open = form.trash_open;
        let state = form.pool(pool);
        if trash_open && state.trash.needs_load() {
            state.trash.start(rclone, args::trash_list(pool));
        }
        busy |= state.trash.poll();
    }
    if let Some(panel) = form.versions.as_mut() {
        if panel.fetch.needs_load() {
            panel
                .fetch
                .start(rclone, args::versions_list(&panel.pool, &panel.path));
        }
        busy |= panel.fetch.poll();
    }
    if let Some(dialog) = form.rollback.as_mut() {
        busy |= dialog.preview.poll();
    }
    busy
}

/// Opens what the explorer asked for.
pub(crate) fn open(form: &mut HistoryForm, pool: &str, request: HistoryRequest) {
    match request {
        HistoryRequest::Versions(path) => form.open_versions(pool, paths::to_drive(&path)),
        HistoryRequest::Rollback(folder) => {
            let now = crate::utils::now_unix();
            let custom = clock::format_local(now.saturating_sub(3_600), clock::offset());
            form.open_rollback(pool, paths::to_drive(&folder), custom);
        }
    }
}

/// The modal dialogs (trash confirmations, rollback) of the open pool.
pub(crate) fn dialogs(
    ctx: &egui::Context,
    form: &mut HistoryForm,
    pool: &str,
    tree: Option<&DriveTree>,
    task: &mut TaskRunner,
    rclone: &str,
) {
    trash_dialogs::show(ctx, form, pool, tree, task, rclone);
    rollback_dialog::show(ctx, form, task, rclone);
}
