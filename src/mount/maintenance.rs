//! Background upkeep of a mounted drive. Uploads, the metadata pull and
//! capacity reporting run in separate workers, so a long upload never leaves
//! the capacity snapshot (and with it the OS free-space report) stale, and an
//! acknowledged write never waits for a metadata pull or the interval: the
//! uploader (`upload_worker`) is woken by the write itself. Each periodic
//! worker runs at most once at a time.
//! An idle mount therefore downloads only metadata each interval: the event
//! listings of the pull (records other PCs added are fetched once) and the
//! capacity `about` call. Uploaded shards are read back once; later re-checks in
//! the same process only stat them (`storage::verified`).
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Capacity snapshots expire after 120 s; refresh well inside that window.
const CAPACITY_INTERVAL_MAX: Duration = Duration::from_secs(60);
/// How often idle accounts are checked for an automatic keep-alive (the
/// interval itself is the `keepalive_days` account-limit setting).
const KEEPALIVE_CHECK: Duration = Duration::from_secs(3600);
/// How often waiting drive history requests (`rpool drive …`) are picked up.
const HISTORY_REQUESTS: Duration = Duration::from_secs(2);
/// Automatic drive cleanup (`drive_history::cleanup`): daily, the first pass
/// a while after the mount starts (not during the start-up sync).
const CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 3600);
const CLEANUP_FIRST: Duration = Duration::from_secs(30 * 60);

/// Uploader restarts tolerated within `RESTART_WINDOW` before the mount stops.
const MAX_RESTARTS: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(600);

/// The message of a thread panic, for the log.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "unknown panic".into())
}

struct Periodic {
    interval: Duration,
    last: Option<Instant>,
    job: Option<JoinHandle<()>>,
    created: Instant,
    /// Wait before the first run (when `last` is `None`).
    first: Duration,
}
impl Periodic {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            job: None,
            created: Instant::now(),
            first: Duration::ZERO,
        }
    }
    /// First run `first` after creation, then every `interval`.
    fn first_after(first: Duration, interval: Duration) -> Self {
        Self {
            first,
            ..Self::new(interval)
        }
    }
    fn due(&self) -> bool {
        match self.last {
            Some(t) => t.elapsed() >= self.interval,
            None => self.created.elapsed() >= self.first,
        }
    }
    /// First run one interval after creation.
    fn delayed(interval: Duration) -> Self {
        Self {
            last: Some(Instant::now()),
            ..Self::new(interval)
        }
    }
    fn poll(&mut self, start: impl FnOnce() -> JoinHandle<()>) -> Result<()> {
        if self.job.as_ref().is_some_and(|j| j.is_finished()) {
            self.join()?;
        }
        if self.job.is_none() && self.due() {
            self.job = Some(start());
            self.last = Some(Instant::now());
        }
        Ok(())
    }
    /// Waits for the finished job. A panic is logged and the job runs again
    /// at its next due time: background upkeep failing must never unmount
    /// the drive (all data stays in the workspace).
    fn join(&mut self) -> Result<()> {
        if let Some(job) = self.job.take() {
            if let Err(panic) = job.join() {
                eprintln!(
                    "Background maintenance failed ({}); it will run again. Local data retained.",
                    panic_text(&*panic)
                );
            }
        }
        Ok(())
    }
}

pub(super) struct Maintenance {
    interval: Duration,
    /// Long-lived uploader thread (`upload_worker`), started on the first poll.
    uploader: Option<JoinHandle<()>>,
    /// Long-lived metadata publisher thread (`publisher`).
    publisher: Option<JoinHandle<()>>,
    /// Recent uploader restarts after a panic (`RESTART_WINDOW`).
    uploader_restarts: Vec<Instant>,
    /// Metadata pull of other PCs' changes (formerly the first step of every sync).
    pull: Periodic,
    /// Last pull error, logged once per change.
    pull_error: Arc<Mutex<Option<String>>>,
    capacity: Periodic,
    /// Metadata checkpoint/compaction pass (`metadata_compaction`).
    compaction: Periodic,
    keepalive: Periodic,
    /// Drive history requests of other processes (`drive_history::request`).
    history: Periodic,
    /// Daily drive cleanup pass (`drive_history::cleanup::auto`).
    cleanup: Periodic,
}
impl Maintenance {
    pub(super) fn new(interval: Duration) -> Self {
        let minutes = super::metadata_compaction::Config::load()
            .map(|c| c.interval_minutes)
            .unwrap_or(super::metadata_compaction::Config::default().interval_minutes);
        Self {
            interval,
            uploader: None,
            publisher: None,
            uploader_restarts: Vec::new(),
            // The mount already pulled before it became ready.
            pull: Periodic::delayed(interval),
            pull_error: Arc::new(Mutex::new(None)),
            capacity: Periodic::new(interval.min(CAPACITY_INTERVAL_MAX)),
            compaction: Periodic::delayed(Duration::from_secs(minutes.saturating_mul(60))),
            keepalive: Periodic::new(KEEPALIVE_CHECK),
            history: Periodic::new(HISTORY_REQUESTS),
            cleanup: Periodic::first_after(CLEANUP_FIRST, CLEANUP_INTERVAL),
        }
    }
    /// Starts whichever worker is due; `report` refreshes and publishes capacity.
    pub(super) fn poll(
        &mut self,
        drive: &Arc<VirtualDrive>,
        report: &Arc<dyn Fn() + Send + Sync>,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<()> {
        self.capacity.poll(|| {
            let (report, cancelled) = (report.clone(), cancelled.clone());
            // Quota is optional for reads and must never block the control loop.
            std::thread::spawn(move || {
                if !cancelled.load(Ordering::Acquire) {
                    report();
                }
            })
        })?;
        self.keepalive.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if !cancelled.load(Ordering::Acquire) {
                    let kept = crate::provider::keepalive::run_due(
                        &drive.rclone,
                        &drive.policy.remotes,
                        &cancelled,
                    );
                    if kept > 0 {
                        println!("Kept {kept} idle cloud account(s) alive");
                    }
                }
            })
        })?;
        if self.uploader.as_ref().is_some_and(|j| j.is_finished()) {
            // It only returns on cancellation; anything else is a panic. The
            // drive stays mounted and a new uploader resumes the queue (the
            // pending saves are in the workspace); only a crash loop stops it.
            if let Some(job) = self.uploader.take() {
                if let Err(panic) = job.join() {
                    let now = Instant::now();
                    self.uploader_restarts
                        .retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                    self.uploader_restarts.push(now);
                    eprintln!(
                        "Background upload failed ({}); restarting it. Local data retained.",
                        panic_text(&*panic)
                    );
                    if self.uploader_restarts.len() > MAX_RESTARTS {
                        bail!("background upload keeps failing; local data retained");
                    }
                }
            }
        }
        if !drive.pool_sync_roots.is_empty() {
            if self.publisher.as_ref().is_some_and(|j| j.is_finished()) {
                if let Some(Err(panic)) = self.publisher.take().map(JoinHandle::join) {
                    eprintln!(
                        "Background metadata publication failed ({}); restarting it. Local data retained.",
                        panic_text(&*panic)
                    );
                }
            }
            if self.publisher.is_none() && !cancelled.load(Ordering::Acquire) {
                self.publisher = Some(super::publisher::spawn(
                    drive.clone(),
                    cancelled.clone(),
                    self.interval,
                ));
            }
        }
        if self.uploader.is_none() && !cancelled.load(Ordering::Acquire) {
            self.uploader = Some(super::upload_worker::spawn(
                drive.clone(),
                cancelled.clone(),
                self.interval,
            ));
        }
        self.pull.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            let last = self.pull_error.clone();
            std::thread::spawn(move || {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let error = drive.pull().err().map(|e| format!("{e:#}"));
                let mut last = last.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(text) = &error {
                    if last.as_ref() != Some(text) {
                        eprintln!("Cloud metadata refresh pending: {text}");
                    }
                }
                *last = error;
            })
        })?;
        self.compaction.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if !cancelled.load(Ordering::Acquire) {
                    compact(&drive);
                }
            })
        })?;
        if drive.pool_sync_roots.is_empty() {
            return Ok(());
        }
        self.cleanup.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if !cancelled.load(Ordering::Acquire) {
                    cleanup(&drive, cancelled);
                }
            })
        })?;
        self.history.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                match crate::drive_history::dispatch::serve_mount(&drive) {
                    Ok(0) => {}
                    Ok(n) => println!("Answered {n} drive history request(s)"),
                    Err(e) => eprintln!("Drive history request failed: {e:#}"),
                }
            })
        })
    }
    /// Waits for running workers (after cancellation was requested).
    pub(super) fn join(mut self, drive: &VirtualDrive) -> Result<()> {
        // The uploader may be waiting for a wake-up; it sees the cancellation.
        drive.upload.notify();
        drive.publish.notify();
        if let Some(job) = self.publisher.take() {
            if let Err(panic) = job.join() {
                eprintln!(
                    "Background metadata publication failed ({}).",
                    panic_text(&*panic)
                );
            }
        }
        let upload = match self.uploader.take() {
            Some(job) => job
                .join()
                .map_err(|_| anyhow!("background upload panicked; local data retained")),
            None => Ok(()),
        };
        let sync = self.pull.join().and(upload);
        let capacity = self.capacity.join();
        let compaction = self.compaction.join();
        let keepalive = self.keepalive.join();
        let history = self.history.join();
        let cleanup = self.cleanup.join();
        sync.and(capacity)
            .and(compaction)
            .and(keepalive)
            .and(history)
            .and(cleanup)
    }
}

/// One automatic drive cleanup pass; never fails the mount (errors and
/// postponements are only logged; the next pass retries).
fn cleanup(drive: &VirtualDrive, cancelled: Arc<AtomicBool>) {
    match crate::drive_history::cleanup::auto(drive, cancelled) {
        None => {}
        Some(Err(e)) => eprintln!("Drive cleanup pending: {e:#}"),
        Some(Ok(report)) => {
            if let Some(line) = crate::drive_history::cleanup::log_line(&report) {
                eprintln!("{line}");
            }
        }
    }
}

/// One automatic compaction pass; never fails the mount.
fn compact(drive: &VirtualDrive) {
    let result = drive.compact_metadata();
    let now = crate::storage::rclone::traffic::now_unix();
    crate::monitor::runtime::note_metadata(super::metadata_pool::growth_alert(
        &drive.pool,
        &result,
        now,
    ));
    match result {
        Err(e) => eprintln!("Metadata compaction pending: {e:#}"),
        Ok(Some(report)) if report.checkpoint.is_some() || report.deleted > 0 => eprintln!(
            "Metadata compaction: {} records checkpointed, {} covered records deleted",
            report.checkpointed_records, report.deleted
        ),
        Ok(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_refresh_stays_inside_the_snapshot_lifetime() {
        assert_eq!(
            Maintenance::new(Duration::from_secs(30)).capacity.interval,
            Duration::from_secs(30)
        );
        assert_eq!(
            Maintenance::new(Duration::from_secs(3600))
                .capacity
                .interval,
            CAPACITY_INTERVAL_MAX
        );
        let pull = Maintenance::new(Duration::from_secs(3600)).pull;
        assert_eq!(pull.interval, Duration::from_secs(3600));
        assert!(!pull.due(), "the mount pulled before it became ready");
        // Cleanup runs daily, the first pass half an hour after the start.
        let cleanup = &Maintenance::new(Duration::from_secs(30)).cleanup;
        assert_eq!(
            (cleanup.interval, cleanup.first, cleanup.last),
            (CLEANUP_INTERVAL, CLEANUP_FIRST, None)
        );
        assert!(!cleanup.due());
        let mut started = Periodic::first_after(Duration::ZERO, CLEANUP_INTERVAL);
        assert!(started.due());
        started.last = Some(Instant::now());
        assert!(!started.due(), "then it waits a day");
        // Compaction never runs at mount start; it waits one interval.
        assert!(Maintenance::new(Duration::from_secs(30))
            .compaction
            .last
            .is_some());
    }

    #[test]
    fn a_long_sync_does_not_hold_back_capacity_reports() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let reports = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut sync = Periodic::new(Duration::from_millis(1));
        let mut capacity = Periodic::new(Duration::from_millis(1));
        sync.poll(|| {
            std::thread::spawn(move || {
                let _ = blocked.recv();
            })
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while reports.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
            let counter = reports.clone();
            capacity
                .poll(|| {
                    std::thread::spawn(move || {
                        counter.fetch_add(1, Ordering::SeqCst);
                    })
                })
                .unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            reports.load(Ordering::SeqCst) >= 3,
            "capacity kept refreshing"
        );
        assert!(
            sync.job.as_ref().is_some_and(|j| !j.is_finished()),
            "the long sync is still running"
        );
        release.send(()).unwrap();
        sync.join().unwrap();
        capacity.join().unwrap();
    }
}
