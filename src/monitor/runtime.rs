//! The monitor thread of one mount process: registers the mount, samples the
//! traffic counters about once per second into `net-status.json`, appends
//! the per-minute history and cleans everything up on a clean exit.
use super::files::{history_dir, status_path, write_json_atomic};
use super::model::MountEntry;
use super::sampler::{PendingItem, Sampler};
use super::{history, registry};
use crate::storage::rclone::traffic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(1);
const STEP: Duration = Duration::from_millis(100);
const PRUNE_EVERY: u64 = 3600;
const BACKEND_RETRY: u64 = 60;

/// Last successful metadata publish of this process's mount (0 = never).
static LAST_SYNC: AtomicU64 = AtomicU64::new(0);

/// Records a successful publish of the pool's metadata to the cloud.
pub(crate) fn note_sync() {
    LAST_SYNC.fetch_max(traffic::now_unix(), Ordering::Relaxed);
}
fn last_sync() -> Option<u64> {
    Some(LAST_SYNC.load(Ordering::Relaxed)).filter(|at| *at != 0)
}

/// Latest metadata-compaction alert of this process's mount (`None`: fine).
static METADATA_ALERT: std::sync::Mutex<Option<super::model::Alert>> = std::sync::Mutex::new(None);

/// Records the outcome of the mount's last metadata compaction pass.
pub(crate) fn note_metadata(alert: Option<super::model::Alert>) {
    if let Ok(mut current) = METADATA_ALERT.lock() {
        // Keep the first time a still-present condition was seen.
        *current = match (current.take(), alert) {
            (Some(old), Some(mut new)) if old.kind == new.kind => {
                new.since_unix = old.since_unix;
                Some(new)
            }
            (_, alert) => alert,
        };
    }
}
fn metadata_alert() -> Option<super::model::Alert> {
    METADATA_ALERT.lock().ok().and_then(|a| a.clone())
}

/// The drive's pending uploads, sampled each tick.
pub(crate) type QueueFn = Box<dyn Fn() -> Vec<PendingItem> + Send>;

/// What a mount tells its monitor.
pub(crate) struct Spec {
    pub pool: String,
    pub workspace: PathBuf,
    pub mountpoint: String,
    /// `fuse`, `winfsp` or `dav`.
    pub frontend: String,
    /// Pool remote addresses, as in the pool definition.
    pub remotes: Vec<String>,
    pub rclone: String,
}

/// Running monitor; stops, writes the last history and removes the status
/// file and registry entry when dropped.
pub(crate) struct MountMonitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    status: PathBuf,
    entry: Option<PathBuf>,
}

impl MountMonitor {
    /// Starts the monitor. Monitoring problems never fail the mount; they
    /// are reported on stderr.
    pub(crate) fn start(spec: Spec, queue: QueueFn) -> Self {
        let started = traffic::now_unix();
        let workspace = std::path::absolute(&spec.workspace).unwrap_or(spec.workspace.clone());
        let entry = MountEntry {
            id: registry::new_id(),
            pool: spec.pool.clone(),
            workspace: workspace.to_string_lossy().into_owned(),
            mountpoint: spec.mountpoint.clone(),
            frontend: spec.frontend.clone(),
            pid: std::process::id(),
            started_unix: started,
        };
        let entry = match registry::registry_dir().and_then(|dir| registry::register(&dir, &entry))
        {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("Mount monitor registration unavailable: {error:#}");
                None
            }
        };
        Self::start_with(spec, workspace, started, queue, entry, TICK)
    }

    pub(crate) fn start_with(
        spec: Spec,
        workspace: PathBuf,
        started: u64,
        queue: QueueFn,
        entry: Option<PathBuf>,
        tick: Duration,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let status = status_path(&workspace);
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rpool-monitor".into())
            .spawn(move || run(spec, &workspace, started, queue, &flag, tick))
            .map_err(|error| eprintln!("Mount monitor unavailable: {error}"))
            .ok();
        Self {
            stop,
            thread,
            status,
            entry,
        }
    }
}

impl Drop for MountMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.status);
        if let Some(entry) = &self.entry {
            let _ = std::fs::remove_file(entry);
        }
    }
}

fn backends(rclone: &str, remotes: &[String]) -> Option<Vec<Option<String>>> {
    use crate::storage::traits::OperationContext;
    let context = crate::storage::rclone::RcloneContext::inherited(rclone);
    let ctx = OperationContext::with_deadline(std::time::Instant::now() + Duration::from_secs(20));
    let config = context.config_dump(&ctx).ok()?;
    Some(
        remotes
            .iter()
            .map(|remote| crate::speedtest::backend_type(&config, remote))
            .collect(),
    )
}

fn mtime(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn run(
    spec: Spec,
    workspace: &Path,
    started: u64,
    queue: QueueFn,
    stop: &AtomicBool,
    tick: Duration,
) {
    let mut sampler = Sampler::new(&spec.pool, spec.remotes.clone(), started);
    let status = status_path(workspace);
    let history = history_dir(workspace);
    let mut next_prune = 0;
    let mut next_backends = 0;
    let mut reported = false;
    loop {
        let now = traffic::now_unix();
        if !sampler.has_backends() && now >= next_backends {
            if let Some(found) = backends(&spec.rclone, sampler.remotes()) {
                sampler.set_backends(found);
            }
            next_backends = now + BACKEND_RETRY;
        }
        let counters: Vec<_> = sampler
            .remotes()
            .iter()
            .map(|remote| traffic::snapshot_at(remote, now))
            .collect();
        let (mut net, points) = sampler.sample(now, &counters, &queue(), last_sync(), mtime);
        net.alerts.extend(metadata_alert());
        net.alerts
            .extend(super::upload_limit::current(now, sampler.remotes()));
        let mut result = write_json_atomic(&status, &net);
        if result.is_ok() {
            result = history::append(&history, &points);
        }
        if now >= next_prune {
            let _ = history::prune(&history, now);
            next_prune = now + PRUNE_EVERY;
        }
        match result {
            Err(error) if !reported => {
                eprintln!("Mount monitor status write failed: {error}");
                reported = true;
            }
            Ok(()) => reported = false,
            Err(_) => {}
        }
        let mut waited = Duration::ZERO;
        while waited < tick {
            if stop.load(Ordering::Acquire) {
                let _ = history::append(&history, &sampler.flush());
                return;
            }
            std::thread::sleep(STEP.min(tick));
            waited += STEP.min(tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::files::read_status_at;
    use std::sync::Mutex;

    #[test]
    fn monitor_thread_writes_status_with_queue_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().to_path_buf();
        std::fs::create_dir_all(workspace.join(".rpool")).unwrap();
        let entry = root.path().join("entry.json");
        std::fs::write(&entry, b"{}").unwrap();
        let remote = "monitor-runtime-test:";
        let pending = Arc::new(Mutex::new(vec![PendingItem {
            id: "i".into(),
            size: 42,
            spool: None,
        }]));
        let shared = pending.clone();
        let op = traffic::Op::begin(Some("monitor-runtime-test"), traffic::Direction::Upload);
        op.sent(1234);
        op.acked(1234);
        op.finish(&Ok::<(), crate::storage::error::StorageError>(()));
        let monitor = MountMonitor::start_with(
            Spec {
                pool: "p".into(),
                workspace: workspace.clone(),
                mountpoint: "/mnt".into(),
                frontend: "dav".into(),
                remotes: vec![remote.into(), "monitor-runtime-idle:".into()],
                rclone: "/nonexistent/rclone".into(),
            },
            workspace.clone(),
            traffic::now_unix(),
            Box::new(move || shared.lock().unwrap().clone()),
            Some(entry.clone()),
            Duration::from_millis(50),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = read_status_at(&workspace, traffic::now_unix()) {
                if status.queue.pending_files == 0 {
                    break status;
                }
                assert_eq!(status.queue.pending_bytes, 42);
                pending.lock().unwrap().clear();
            }
            assert!(std::time::Instant::now() < deadline, "no status written");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.pool, "p");
        assert_eq!(status.remotes.len(), 2);
        assert_eq!(status.remotes[0].sent_bytes, 1234);
        assert_eq!(status.remotes[0].acked_bytes, 1234);
        assert_eq!(status.remotes[0].ok_ops, 1);
        assert_eq!(status.remotes[1].ok_ops, 0);
        assert_eq!(status.remotes[0].backend, None);
        drop(monitor);
        assert!(!status_path(&workspace).exists());
        assert!(!entry.exists());
        // The unfinished minute was written on stop.
        let points = history::load(&history_dir(&workspace), 0);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].remote, remote);
    }

    #[test]
    fn metadata_alert_keeps_its_first_time_and_clears() {
        let alert = |since| crate::monitor::model::Alert {
            kind: crate::monitor::model::AlertKind::MetadataGrowing,
            remote: None,
            since_unix: since,
            message: "growing".into(),
        };
        note_metadata(Some(alert(100)));
        note_metadata(Some(alert(200)));
        assert_eq!(metadata_alert().unwrap().since_unix, 100);
        note_metadata(None);
        assert!(metadata_alert().is_none());
    }

    #[test]
    fn note_sync_is_reported() {
        note_sync();
        assert!(last_sync().is_some());
    }
}
