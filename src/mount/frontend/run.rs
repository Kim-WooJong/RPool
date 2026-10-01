//! Lifecycle of a native mount: mount, periodic background sync, stop file,
//! unmount. Cloud sync never gates the frontend's acknowledgements.
use super::super::fs_core::FsCore;
use super::super::lifecycle::StopControl;
use super::super::virtual_drive::VirtualDrive;
use crate::cli::Frontend;
use crate::prelude::*;
use std::time::Duration;

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
        Frontend::Dav | Frontend::Auto => bail!("not a native frontend"),
        #[cfg(target_os = "linux")]
        Frontend::Fuse => super::fuse::mount(core, mountpoint, read_only),
        #[cfg(not(target_os = "linux"))]
        Frontend::Fuse => bail!("the FUSE frontend is available on Linux only"),
        #[cfg(all(windows, feature = "winfsp"))]
        Frontend::Winfsp => super::winfsp::mount(core, mountpoint, read_only),
        #[cfg(not(all(windows, feature = "winfsp")))]
        Frontend::Winfsp => bail!(
            "the WinFsp frontend needs a Windows build with the `winfsp` feature (on by default) and WinFsp installed"
        ),
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
    let mut maintenance = super::super::maintenance::Maintenance::new(run.interval);
    let outcome: Result<()> = (|| loop {
        if run.stop.requested() {
            return Ok(());
        }
        if !mount.alive() {
            bail!("native filesystem was unmounted externally; local data retained");
        }
        if !run.read_only {
            maintenance.poll(&drive, &run.report, &run.stop.flag)?;
        }
        std::thread::sleep(Duration::from_millis(250));
    })();
    run.stop.cancel();
    println!("Unmounting native filesystem; pending local data will be retained");
    let stopped = mount.stop();
    let joined = maintenance.join(&drive);
    stopped?;
    joined?;
    println!("Native mount stopped; pending spool/cache/history retained. Cloud replication was not drained.");
    outcome
}
