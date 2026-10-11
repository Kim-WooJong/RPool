//! Samples the migration CLI process's per-storage counters for its GUI parent.
use super::{emitter, StorageTraffic};
use crate::storage::rclone::traffic;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One process's traffic sampler. Dropping it sends the final totals.
pub(crate) struct TrafficReporter {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl TrafficReporter {
    pub(crate) fn start() -> Self {
        if !super::enabled() {
            return Self {
                stop: None,
                worker: None,
            };
        }
        let (stop, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut previous = Vec::new();
            loop {
                let stopping = receiver.recv_timeout(Duration::from_secs(1)).is_ok();
                let remotes = snapshot();
                if stopping || remotes != previous {
                    // Small batches keep each progress line below the runner's 16 KiB limit.
                    for batch in remotes.chunks(32) {
                        emitter::traffic(batch.to_vec());
                    }
                    previous = remotes;
                }
                if stopping {
                    break;
                }
            }
        });
        Self {
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}

impl Drop for TrafficReporter {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn snapshot() -> Vec<StorageTraffic> {
    traffic::snapshot_all(traffic::now_unix())
        .into_iter()
        .map(|(remote, value)| StorageTraffic {
            remote,
            sent_bytes: value.sent_bytes,
            received_bytes: value.received_bytes,
            server_side_bytes: value.server_side_bytes,
            upload_rate_10s: value.upload_rate_10s,
            download_rate_10s: value.download_rate_10s,
            active_uploads: value.active_uploads,
            active_downloads: value.active_downloads,
            failed_ops: value.failed_ops,
            retries: value.retries,
            last_upload_error: value.last_upload_error,
            last_upload_storage_full: value.last_upload_storage_full,
            capacity_no_space: value.capacity_no_space_unix.is_some(),
        })
        .collect()
}
