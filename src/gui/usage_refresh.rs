use crate::models::QuotaReport;
use crate::storage::admin::{collect_quota_reports, BackendAdmin, RcloneAdmin};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

pub(crate) struct UsageSnapshot {
    pub(crate) reports: Vec<QuotaReport>,
    pub(crate) crypt_remotes: Vec<String>,
}

pub(crate) struct UsageRefresh {
    receiver: Option<Receiver<Result<UsageSnapshot, String>>>,
    running: bool,
}

impl Default for UsageRefresh {
    fn default() -> Self {
        Self {
            receiver: None,
            running: false,
        }
    }
}

impl UsageRefresh {
    pub(crate) fn start(&mut self, rclone: String, workers: usize) {
        self.start_with_admin(Arc::new(RcloneAdmin::inherited(&rclone)), workers);
    }
    pub(crate) fn start_with_admin(&mut self, admin: Arc<dyn BackendAdmin>, workers: usize) {
        if self.running {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = (|| {
                let catalog = admin
                    .catalog()
                    .map_err(|error| format!("remote discovery failed: {error:#}"))?;
                let physical = catalog
                    .physical_remotes()
                    .map_err(|error| format!("capacity discovery failed: {error:#}"))?;
                let capacity_remotes = catalog
                    .capacity_remotes(&physical)
                    .map_err(|error| format!("capacity mapping failed: {error:#}"))?;
                let crypt_remotes = catalog.crypt_remotes();
                if capacity_remotes.is_empty() {
                    return Err(
                        "rclone has no physical remotes available for capacity reporting"
                            .to_string(),
                    );
                }
                let reports =
                    collect_quota_reports(admin.as_ref(), &capacity_remotes, workers.max(1))
                        .map_err(|error| format!("failed to query provider usage: {error:#}"))?;
                Ok(UsageSnapshot {
                    reports,
                    crypt_remotes,
                })
            })();
            let _ = sender.send(result);
        });

        self.receiver = Some(receiver);
        self.running = true;
    }

    pub(crate) fn poll(&mut self) -> Option<Result<UsageSnapshot, String>> {
        let result = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("usage refresh worker stopped unexpectedly".to_string())
            }
        };
        self.receiver = None;
        self.running = false;
        Some(result)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::admin::RemoteCatalog;
    use std::sync::{mpsc, Barrier};
    struct Blocked {
        entered: mpsc::Sender<()>,
        release: Arc<Barrier>,
        finished: mpsc::Sender<()>,
    }
    impl BackendAdmin for Blocked {
        fn catalog(&self) -> anyhow::Result<RemoteCatalog> {
            self.entered.send(()).unwrap();
            self.release.wait();
            self.finished.send(()).unwrap();
            anyhow::bail!("synthetic discovery failure")
        }
        fn discover(&self) -> anyhow::Result<Vec<String>> {
            panic!("unexpected")
        }
        fn probe(&self, _: &str) -> anyhow::Result<()> {
            panic!("unexpected")
        }
        fn quota(&self, _: &str) -> QuotaReport {
            panic!("unexpected")
        }
        fn ensure_encrypted(&self, _: &str) -> anyhow::Result<()> {
            panic!("unexpected")
        }
    }
    #[test]
    fn usage_discovery_stays_off_ui_thread_and_failure_clears_running() {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let gate = Arc::new(Barrier::new(2));
        let mut refresh = UsageRefresh::default();
        refresh.start_with_admin(
            Arc::new(Blocked {
                entered: tx,
                release: gate.clone(),
                finished: done_tx,
            }),
            1,
        );
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(refresh.is_running());
        assert!(refresh.poll().is_none());
        gate.wait();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // Receiver blocks only in test; production poll remains nonblocking.
        let result = refresh
            .receiver
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(result.is_err());
        // Replay the received result to exercise poll deterministically. The worker
        // may still own its sender after send; disconnection is not a join signal.
        let (tx, rx) = mpsc::channel();
        tx.send(result).unwrap();
        refresh.receiver = Some(rx);
        assert!(refresh.poll().unwrap().is_err());
        assert!(!refresh.is_running());
    }
}
