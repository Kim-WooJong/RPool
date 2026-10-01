//! Background upkeep of a mounted drive. `sync` and capacity reporting run in
//! separate workers, so a long upload never leaves the capacity snapshot (and
//! with it the OS free-space report) stale. Each worker runs at most once at a time.
//! An idle mount therefore downloads only metadata each interval: the event
//! listings of the sync poll (records other PCs added are fetched once) and the
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
    /// Metadata checkpoint/compaction pass (`metadata_compaction`).
    compaction: Periodic,
    keepalive: Periodic,
}
impl Maintenance {
    pub(super) fn new(interval: Duration) -> Self {
        let minutes = super::metadata_compaction::Config::load()
            .map(|c| c.interval_minutes)
            .unwrap_or(super::metadata_compaction::Config::default().interval_minutes);
        Self {
            sync: Periodic::new(interval),
            capacity: Periodic::new(interval.min(CAPACITY_INTERVAL_MAX)),
            compaction: Periodic::delayed(Duration::from_secs(minutes.saturating_mul(60))),
            keepalive: Periodic::new(KEEPALIVE_CHECK),
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
        })?;
        self.compaction.poll(|| {
            let (drive, cancelled) = (drive.clone(), cancelled.clone());
            std::thread::spawn(move || {
                if !cancelled.load(Ordering::Acquire) {
                    compact(&drive);
                }
            })
        })
    }
    /// Waits for running workers (after cancellation was requested).
    pub(super) fn join(mut self) -> Result<()> {
        let sync = self.sync.join();
        let capacity = self.capacity.join();
        let compaction = self.compaction.join();
        let keepalive = self.keepalive.join();
        sync.and(capacity).and(compaction).and(keepalive)
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
        assert_eq!(
            Maintenance::new(Duration::from_secs(3600)).sync.interval,
            Duration::from_secs(3600)
        );
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
