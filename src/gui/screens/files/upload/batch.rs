//! Sequential upload batch: starts one `rpool put` per pending queue item and
//! moves on when the task runner reports the previous one finished.

use super::state::UploadItemStatus;
use super::{start, validation};
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;

/// Starts the batch after checking that no task runs, the queue has pending
/// files and the preflight passes. Called by the Start upload button.
pub(crate) fn start_batch(state: &mut GuiState, task: &mut TaskRunner) -> Result<(), String> {
    if task.is_running() {
        return Err(tr("Another rpool operation is already running.").to_string());
    }
    if state.upload.batch_active {
        return Err(tr("This upload batch is already running.").to_string());
    }
    if state.upload.items.is_empty() {
        return Err(tr("Add one or more source files first.").to_string());
    }
    if state.upload.pending_count() == 0 {
        return Err(tr("There are no pending files to upload.").to_string());
    }
    validation::validate_configuration(state)?;
    state.upload.error = None;
    state.upload.batch_active = true;
    start_next(state, task)
}

/// Advances an active batch while the task runner is idle: records the last
/// outcome on the running item, stops on cancel or failure (pending items stay
/// queued), otherwise starts the next item. Called every frame from the app
/// through `upload::poll_batch`.
pub(crate) fn poll_batch(state: &mut GuiState, task: &mut TaskRunner) {
    if !state.upload.batch_active || task.is_running() {
        return;
    }

    let Some(index) = state.upload.active_index else {
        if let Err(error) = start_next(state, task) {
            state.upload.batch_active = false;
            state.upload.error = Some(error);
        }
        return;
    };

    let Some(outcome) = task.last_outcome().cloned() else {
        return;
    };

    if let Some(item) = state.upload.items.get_mut(index) {
        item.status = if outcome.cancelled {
            UploadItemStatus::Cancelled
        } else if outcome.success {
            UploadItemStatus::Completed
        } else {
            UploadItemStatus::Failed
        };
    }
    state.upload.active_index = None;

    if outcome.cancelled {
        state.upload.batch_active = false;
        state.upload.error =
            Some(tr("Upload batch cancelled. Pending files were kept in the queue.").to_string());
        return;
    }
    if !outcome.success {
        state.upload.batch_active = false;
        state.upload.error = Some(
            tr("Upload batch paused after a failed file. Pending files were kept in the queue.")
                .to_string(),
        );
        return;
    }

    if let Err(error) = start_next(state, task) {
        state.upload.batch_active = false;
        state.upload.error = Some(error);
    }
}

/// Starts the first pending item, marking it Running; ends the batch when none
/// is left. On a start error the item is marked Failed and the batch stops.
fn start_next(state: &mut GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let Some(index) = state
        .upload
        .items
        .iter()
        .position(|item| item.status == UploadItemStatus::Pending)
    else {
        state.upload.batch_active = false;
        state.upload.active_index = None;
        state.upload.error = None;
        return Ok(());
    };

    let path = state.upload.items[index].path.clone();
    state.upload.items[index].status = UploadItemStatus::Running;
    state.upload.active_index = Some(index);

    if let Err(error) = start::start_upload_path(state, task, &path) {
        state.upload.items[index].status = UploadItemStatus::Failed;
        state.upload.active_index = None;
        state.upload.batch_active = false;
        return Err(error);
    }
    Ok(())
}
