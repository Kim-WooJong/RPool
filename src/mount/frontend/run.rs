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
/// Mounts `core` with the requested native frontend; errors for frontends unavailable on this OS.
fn start(
    frontend: Frontend,
    core: Arc<FsCore>,
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    match frontend {
        Frontend::Dav | Frontend::Auto => bail!("not a native frontend"),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Frontend::Fuse => super::fuse::mount(core, mountpoint, volume, read_only),
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Frontend::Fuse => bail!("the FUSE frontend is available on Linux and macOS (macFUSE) only"),
        #[cfg(all(windows, feature = "winfsp"))]
        Frontend::Winfsp => super::winfsp::mount(core, mountpoint, read_only),
        #[cfg(not(all(windows, feature = "winfsp")))]
        Frontend::Winfsp => bail!(
            "the WinFsp frontend needs a Windows build with the `winfsp` feature (on by default) and WinFsp installed"
        ),
    }
}

/// Context marking an error from mounting a native frontend, before anything
/// was mounted (`--frontend auto` may then use WebDAV).
#[derive(Debug)]
struct NativeStartFailed;
impl std::fmt::Display for NativeStartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native frontend could not mount")
    }
}

/// Whether `error` came from a native frontend failing to mount.
pub(crate) fn native_start_failed(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NativeStartFailed>().is_some()
}

/// Parameters of [`run_native`], built by `virtual_drive::run`.
pub(crate) struct NativeRun<'a> {
    /// Native frontend to mount (FUSE or WinFsp).
    pub(crate) frontend: Frontend,
    /// Where to mount.
    pub(crate) mountpoint: &'a Path,
    /// Volume name shown by the OS where the frontend supports one (macOS).
    pub(crate) volume_name: &'a str,
    /// Serve without writes; background maintenance is skipped.
    pub(crate) read_only: bool,
    /// Background maintenance interval.
    pub(crate) interval: Duration,
    /// Stop request and cancellation of the session.
    pub(crate) stop: &'a StopControl,
    /// Refreshes and publishes capacity (passed to `Maintenance::poll`).
    pub(crate) report: Arc<dyn Fn() + Send + Sync>,
}

/// Mounts a native frontend over `drive`, runs background maintenance until a stop request
/// or external unmount, then unmounts and joins the workers. Mount errors are tagged
/// [`NativeStartFailed`] so `--frontend auto` can fall back to WebDAV.
pub(crate) fn run_native(drive: Arc<VirtualDrive>, run: NativeRun<'_>) -> Result<()> {
    let core = Arc::new(FsCore::new(drive.clone()).map_err(|e| anyhow!("{e}"))?);
    let mount = start(
        run.frontend,
        core,
        run.mountpoint,
        run.volume_name,
        run.read_only,
    )
    .context(NativeStartFailed)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mount_start_failures_are_marked() {
        let start = anyhow!("macFUSE mount failed").context(NativeStartFailed);
        assert!(native_start_failed(&start));
        assert!(native_start_failed(&start.context("outer")));
        assert!(!native_start_failed(&anyhow!(
            "native filesystem was unmounted externally"
        )));
    }
}
