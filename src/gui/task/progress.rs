//! Applies child-process `ProgressEvent`s to a [`TaskProgress`] and derives
//! the transfer rate and ETA. Owned by `super::runner::TaskRunner`.

use super::TaskProgress;
use crate::progress::ProgressEvent;
use std::time::Instant;

/// Rate/ETA state for the current task; reset when a new task starts.
#[derive(Debug, Default)]
pub(crate) struct ProgressTracker {
    /// When byte progress started (set by `Start`); `None` disables rate/ETA.
    started_at: Option<Instant>,
    /// Bytes transferred since `started_at`, used for the average rate.
    transferred_for_rate: u64,
}

impl ProgressTracker {
    /// Clears timing state before a new task or when the log is cleared.
    pub(crate) fn reset(&mut self) {
        self.started_at = None;
        self.transferred_for_rate = 0;
    }

    /// Updates `progress` from one event: `Start` sets byte totals, `Advance` adds
    /// deltas (completed capped at total), `Items` switches to item counting and
    /// `Finish` marks the totals as reached.
    pub(crate) fn apply(&mut self, progress: &mut TaskProgress, event: ProgressEvent) {
        match event {
            ProgressEvent::Start { total_bytes } => {
                progress.completed_bytes = Some(0);
                progress.transferred_bytes = Some(0);
                progress.total_bytes = Some(total_bytes);
                progress.bytes_per_second = None;
                progress.eta_seconds = None;
                progress.current_item = None;
                progress.total_items = None;
                self.started_at = Some(Instant::now());
                self.transferred_for_rate = 0;
            }
            ProgressEvent::Advance {
                completed_bytes,
                transferred_bytes,
            } => {
                let completed = progress
                    .completed_bytes
                    .unwrap_or(0)
                    .saturating_add(completed_bytes);
                let transferred = progress
                    .transferred_bytes
                    .unwrap_or(0)
                    .saturating_add(transferred_bytes);
                progress.completed_bytes = Some(
                    progress
                        .total_bytes
                        .map(|total| completed.min(total))
                        .unwrap_or(completed),
                );
                progress.transferred_bytes = Some(transferred);
                self.transferred_for_rate =
                    self.transferred_for_rate.saturating_add(transferred_bytes);
                self.update_rate_and_eta(progress);
            }
            ProgressEvent::Items {
                completed_items,
                total_items,
            } => {
                progress.completed_bytes = None;
                progress.transferred_bytes = None;
                progress.total_bytes = None;
                progress.bytes_per_second = None;
                progress.eta_seconds = None;
                let completed = completed_items.min(total_items);
                progress.current_item = Some(if completed_items == 0 {
                    0
                } else {
                    progress.current_item.unwrap_or(0).max(completed)
                });
                progress.total_items = Some(total_items);
                self.started_at = None;
                self.transferred_for_rate = 0;
            }
            ProgressEvent::Traffic { remotes } => {
                for remote in remotes {
                    if let Some(previous) = progress
                        .storage_traffic
                        .iter_mut()
                        .find(|r| r.remote == remote.remote)
                    {
                        *previous = remote;
                    } else {
                        progress.storage_traffic.push(remote);
                    }
                }
                progress
                    .storage_traffic
                    .sort_by(|a, b| a.remote.cmp(&b.remote));
            }
            ProgressEvent::Finish => {
                if let Some(total) = progress.total_bytes {
                    progress.completed_bytes = Some(total);
                    progress.eta_seconds = Some(0);
                }
                if let Some(total) = progress.total_items {
                    progress.current_item = Some(total);
                }
                self.update_rate_and_eta(progress);
            }
        }
    }

    /// Recomputes the average rate and ETA from elapsed time; no-op until bytes moved.
    fn update_rate_and_eta(&self, progress: &mut TaskProgress) {
        let Some(started_at) = self.started_at else {
            return;
        };
        let elapsed = started_at.elapsed().as_secs_f64();
        if elapsed <= 0.0 || self.transferred_for_rate == 0 {
            return;
        }

        let rate = self.transferred_for_rate as f64 / elapsed;
        progress.bytes_per_second = Some(rate);

        if let (Some(total), Some(completed)) = (progress.total_bytes, progress.completed_bytes) {
            let remaining = total.saturating_sub(completed);
            progress.eta_seconds = if remaining == 0 {
                Some(0)
            } else if rate > 0.0 {
                Some((remaining as f64 / rate).ceil() as u64)
            } else {
                None
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::StorageTraffic;

    #[test]
    fn traffic_batches_merge_and_item_events_do_not_erase_them() {
        let row = |remote: &str, sent_bytes| StorageTraffic {
            remote: remote.into(),
            sent_bytes,
            received_bytes: 0,
            server_side_bytes: 0,
            upload_rate_10s: 0.0,
            download_rate_10s: 0.0,
            active_uploads: 0,
            active_downloads: 0,
            failed_ops: 0,
            retries: 0,
            last_upload_error: None,
            last_upload_storage_full: false,
            capacity_no_space: false,
        };
        let mut tracker = ProgressTracker::default();
        let mut progress = TaskProgress::default();
        tracker.apply(
            &mut progress,
            ProgressEvent::Traffic {
                remotes: vec![row("z", 1)],
            },
        );
        tracker.apply(
            &mut progress,
            ProgressEvent::Traffic {
                remotes: vec![row("a", 2)],
            },
        );
        tracker.apply(
            &mut progress,
            ProgressEvent::Traffic {
                remotes: vec![row("z", 3)],
            },
        );
        tracker.apply(
            &mut progress,
            ProgressEvent::Items {
                completed_items: 1,
                total_items: 2,
            },
        );
        assert_eq!(progress.storage_traffic, vec![row("a", 2), row("z", 3)]);
    }
}
