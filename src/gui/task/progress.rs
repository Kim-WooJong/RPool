use super::TaskProgress;
use crate::progress::ProgressEvent;
use std::time::Instant;

#[derive(Debug, Default)]
pub(crate) struct ProgressTracker {
    started_at: Option<Instant>,
    transferred_for_rate: u64,
}

impl ProgressTracker {
    pub(crate) fn reset(&mut self) {
        self.started_at = None;
        self.transferred_for_rate = 0;
    }

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
                let completed = progress.completed_bytes.unwrap_or(0).saturating_add(completed_bytes);
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
                self.transferred_for_rate = self
                    .transferred_for_rate
                    .saturating_add(transferred_bytes);
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
