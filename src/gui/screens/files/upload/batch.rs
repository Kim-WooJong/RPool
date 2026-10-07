//! Upload batch: one task that runs `rpool put` for every pending queue item
//! in turn on the task runner's worker thread (`start_rpool_sequence`), so
//! the batch keeps going while the window is minimized or hidden. The screen
//! only mirrors the step results into the queue.

use super::state::UploadItemStatus;
use super::{start, validation};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::{StepUpdate, TaskRunner};

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
    let indices: Vec<usize> = state
        .upload
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status == UploadItemStatus::Pending)
        .map(|(index, _)| index)
        .collect();
    let steps = indices
        .iter()
        .map(|index| start::upload_args(state, &state.upload.items[*index].path))
        .collect::<Result<Vec<_>, String>>()?;
    let name = match indices.as_slice() {
        [one] => {
            let path = &state.upload.items[*one].path;
            let file = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            format!("Upload {file}")
        }
        _ => trf("Upload {n} files", &[("n", &indices.len())]),
    };
    task.start_rpool_sequence(name, &state.settings.rclone, steps)?;
    state.upload.error = None;
    state.upload.batch_active = true;
    state.upload.batch_items = indices;
    state.upload.active_index = None;
    Ok(())
}

/// Mirrors the batch task's step results into the queue and ends the batch
/// when the task has finished: a cancelled or failed file stops it (later
/// files stay pending). Called every frame from the app through
/// `upload::poll_batch`; the uploads themselves do not wait for it.
pub(crate) fn poll_batch(state: &mut GuiState, task: &mut TaskRunner) {
    if !state.upload.batch_active {
        return;
    }
    let mut stopped = None;
    for update in task.take_step_updates() {
        match update {
            StepUpdate::Started(step) => {
                if let Some(&index) = state.upload.batch_items.get(step) {
                    state.upload.items[index].status = UploadItemStatus::Running;
                    state.upload.active_index = Some(index);
                }
            }
            StepUpdate::Finished(step, outcome) => {
                if let Some(&index) = state.upload.batch_items.get(step) {
                    state.upload.items[index].status = if outcome.cancelled {
                        UploadItemStatus::Cancelled
                    } else if outcome.success {
                        UploadItemStatus::Completed
                    } else {
                        UploadItemStatus::Failed
                    };
                }
                state.upload.active_index = None;
                if !outcome.success {
                    stopped = Some(outcome.cancelled);
                }
            }
        }
    }
    if task.is_running() {
        return;
    }
    state.upload.batch_active = false;
    state.upload.active_index = None;
    state.upload.batch_items.clear();
    let cancelled = stopped.or_else(|| {
        task.last_outcome()
            .filter(|o| !o.success)
            .map(|o| o.cancelled)
    });
    state.upload.error = match cancelled {
        Some(true) => {
            Some(tr("Upload batch cancelled. Pending files were kept in the queue.").to_string())
        }
        Some(false) => Some(
            tr("Upload batch paused after a failed file. Pending files were kept in the queue.")
                .to_string(),
        ),
        None => None,
    };
}
