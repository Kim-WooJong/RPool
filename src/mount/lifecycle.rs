//! Cancellation scope for the dedicated online-mount CLI process, never the GUI.
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;

/// Stop request of a mount process: set by the optional stop file (polled every 50 ms by a
/// watcher thread) or by [`cancel`](Self::cancel). Created by `virtual_drive::run` and
/// `pool_transition`.
pub(super) struct StopControl {
    /// Shared stop flag; also installed as the process-wide storage cancellation flag.
    pub flag: Arc<AtomicBool>,
    /// Ends the watcher thread when dropped or signalled.
    done: mpsc::Sender<()>,
    /// Stop-file watcher thread, joined on drop.
    watcher: Option<JoinHandle<()>>,
    /// Keeps the process-wide cancellation scope installed for this lifetime.
    _scope: crate::storage::traits::ProcessCancellationGuard,
}

impl StopControl {
    /// Installs the cancellation scope and starts watching `path` (no watcher check when `None`).
    pub fn new(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        let scope = crate::storage::traits::ProcessCancellationGuard::install(flag.clone())?;
        let (done, receiver) = mpsc::channel();
        let observed = flag.clone();
        let watcher = std::thread::spawn(move || loop {
            // The stop file, or SIGTERM/SIGINT/SIGHUP/console close (`shutdown_signal`).
            if path.as_ref().is_some_and(|p| p.exists()) || super::shutdown_signal::requested() {
                observed.store(true, Ordering::Release);
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                _ => break,
            }
        });
        Ok(Self {
            flag,
            done,
            watcher: Some(watcher),
            _scope: scope,
        })
    }
    /// True once a stop was requested.
    pub fn requested(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
    /// Requests a stop (cancels in-flight storage work).
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
}
impl Drop for StopControl {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.done.send(());
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_stop_file_cancels_existing_operations_and_scope_resets() {
        const CHILD: &str = "RPOOL_STOP_CONTROL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let name = format!(
                "{}::startup_stop_file_cancels_existing_operations_and_scope_resets",
                module_path!().split_once("::").unwrap().1
            );
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .status()
                .unwrap()
                .success());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stop");
        let stop = StopControl::new(Some(path.clone())).unwrap();
        let operation = crate::storage::traits::OperationContext::none();
        assert!(!operation.is_cancelled());
        std::fs::write(path, b"stop").unwrap();
        let start = std::time::Instant::now();
        while !operation.is_cancelled() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            operation.is_cancelled(),
            "startup network operation must observe stop file"
        );
        drop(stop);
        assert!(!crate::storage::traits::OperationContext::none().is_cancelled());
    }
}
