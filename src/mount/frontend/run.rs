//! Lifecycle of a native mount: mount, periodic background sync, stop file,
//! unmount. Cloud sync never gates the frontend's acknowledgements.
use super::super::fs_core::FsCore;
use super::super::lifecycle::StopControl;
use super::super::virtual_drive::VirtualDrive;
use crate::cli::Frontend;
use crate::prelude::*;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// A mounted native filesystem.
pub(super) trait NativeMount {
    /// `false` once the OS unmounted it (for example `fusermount -u`).
    fn alive(&self) -> bool;
    /// Unmount and wait for in-flight operations to finish.
    fn stop(self: Box<Self>) -> Result<()>;
}

#[allow(unused_variables, reason = "each frontend exists on one OS")]
fn start(
    frontend: Frontend,
    core: Arc<FsCore>,
    mountpoint: &Path,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    match frontend {
        Frontend::Dav => bail!("DAV is not a native frontend"),
        #[cfg(target_os = "linux")]
        Frontend::Fuse => super::fuse::mount(core, mountpoint, read_only),
        #[cfg(not(target_os = "linux"))]
        Frontend::Fuse => bail!("the FUSE frontend is available on Linux only"),
        Frontend::Winfsp => bail!("the WinFsp frontend is not built yet"),
    }
}

pub(crate) struct NativeRun<'a> {
    pub(crate) frontend: Frontend,
    pub(crate) mountpoint: &'a Path,
    pub(crate) read_only: bool,
    pub(crate) interval: Duration,
    pub(crate) stop: &'a StopControl,
    pub(crate) report: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) fn run_native(drive: Arc<VirtualDrive>, run: NativeRun<'_>) -> Result<()> {
    let core = Arc::new(FsCore::new(drive.clone()).map_err(|e| anyhow!("{e}"))?);
    let mount = start(run.frontend, core, run.mountpoint, run.read_only)?;
    println!(
        "Native {:?} filesystem mounted at {} (read_only={}). fsync/close is the local durability point; cloud replication is asynchronous.",
        run.frontend,
        run.mountpoint.display(),
        run.read_only
    );
    let mut job: Option<std::thread::JoinHandle<()>> = None;
    let mut last: Option<Instant> = None;
    let outcome: Result<()> = (|| loop {
        if run.stop.requested() {
            return Ok(());
        }
        if !mount.alive() {
            bail!("native filesystem was unmounted externally; local data retained");
        }
        if job.as_ref().is_some_and(|j| j.is_finished()) {
            job.take()
                .expect("checked above")
                .join()
                .map_err(|_| anyhow!("background maintenance panicked; local data retained"))?;
        }
        let due = last.is_none_or(|t| t.elapsed() >= run.interval);
        if !run.read_only && job.is_none() && due {
            let drive = drive.clone();
            let report = run.report.clone();
            let cancelled = run.stop.flag.clone();
            job = Some(std::thread::spawn(move || {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = drive.sync() {
                    eprintln!("Virtual sync pending: {e:#}");
                }
                if !cancelled.load(Ordering::Acquire) {
                    report();
                }
            }));
            last = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(250));
    })();
    run.stop.cancel();
    println!("Unmounting native filesystem; pending local data will be retained");
    let stopped = mount.stop();
    let joined = job
        .map(|job| job.join())
        .transpose()
        .map_err(|_| anyhow!("background maintenance panicked during stop; local data retained"));
    stopped?;
    joined?;
    println!("Native mount stopped; pending spool/cache/history retained. Cloud replication was not drained.");
    outcome
}
