//! Background upkeep of a mounted drive. `sync` and capacity reporting run in
//! separate workers, so a long upload never leaves the capacity snapshot (and
//! with it the OS free-space report) stale. Each worker runs at most once at a time.
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Capacity snapshots expire after 120 s; refresh well inside that window.
const CAPACITY_INTERVAL_MAX: Duration = Duration::from_secs(60);

struct Periodic {
    interval: Duration,
    last: Option<Instant>,
    job: Option<JoinHandle<()>>,
}
impl Periodic {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
            job: None,
        }
    }
    fn poll(&mut self, start: impl FnOnce() -> JoinHandle<()>) -> Result<()> {
        if self.job.as_ref().is_some_and(|j| j.is_finished()) {
            self.join()?;
        }
        let due = self.last.is_none_or(|t| t.elapsed() >= self.interval);
        if self.job.is_none() && due {
            self.job = Some(start());
            self.last = Some(Instant::now());
        }
        Ok(())
    }
    fn join(&mut self) -> Result<()> {
        match self.job.take() {
            Some(job) => job
                .join()
                .map_err(|_| anyhow!("background maintenance panicked; local data retained")),
            None => Ok(()),
        }
    }
}

pub(super) struct Maintenance {
    sync: Periodic,
    capacity: Periodic,
}
impl Maintenance {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            sync: Periodic::new(interval),
            capacity: Periodic::new(interval.min(CAPACITY_INTERVAL_MAX)),
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
        self.sync.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                match drive.sync() {
                    Ok(()) => crate::monitor::runtime::note_sync(),
                    Err(e) => eprintln!("Virtual sync pending: {e:#}"),
                }
            })
        })
    }
    /// Waits for running workers (after cancellation was requested).
    pub(super) fn join(mut self) -> Result<()> {
        let sync = self.sync.join();
        let capacity = self.capacity.join();
        sync.and(capacity)
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
        assert_eq!(
            Maintenance::new(Duration::from_secs(3600)).sync.interval,
            Duration::from_secs(3600)
        );
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
