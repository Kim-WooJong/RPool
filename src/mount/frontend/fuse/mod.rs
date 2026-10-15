//! FUSE frontend (native mount M5) over `FsCore`, using `fuser`. On Linux it
//! mounts without libfuse; on macOS it mounts through macFUSE (kernel backend
//! first), loaded at runtime (`macfuse`). The last close (`release`) and
//! `fsync` are the local durability points; per-descriptor `flush` does not
//! seal. Both reply from a worker thread (`detached`) after the seal, so one
//! large file's seal never stalls the session's request loop. On macOS,
//! reads, writes and resizes also reply from workers, within macFUSE's
//! request time limit (`bounded`).
/// Off-loop replies with a deadline (macOS).
#[cfg(target_os = "macos")]
mod bounded;
/// Off-loop seal replies.
mod detached;
/// Core error → errno mapping.
mod errno;
/// `fuser::Filesystem` implementation.
mod filesystem;
/// Inode table.
mod inodes;
#[cfg(target_os = "macos")]
pub(crate) mod macfuse;
#[cfg(test)]
mod tests;

use super::run::NativeMount;
use crate::mount::fs_core::FsCore;
use crate::prelude::*;
use fuser::{BackgroundSession, Config, SessionACL};

/// A running FUSE session; returned by [`mount`] as a `NativeMount`.
pub(super) struct FuseMount {
    /// fuser background session serving the mount.
    session: BackgroundSession,
    #[cfg(target_os = "macos")]
    /// Mountpoint, needed to unmount through macFUSE.
    mountpoint: PathBuf,
}
impl FuseMount {
    /// Unmount and wait for the session thread to finish.
    pub(super) fn umount_and_join(self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.session.umount_and_join()?;
        }
        #[cfg(target_os = "macos")]
        {
            // Join only after the kernel let go; a busy volume keeps serving.
            if !self.session.guard.is_finished() {
                macfuse::unmount(&self.mountpoint)?;
            }
            self.session.join()?;
        }
        Ok(())
    }
}
impl NativeMount for FuseMount {
    fn alive(&self) -> bool {
        !self.session.guard.is_finished()
    }
    fn stop(self: Box<Self>) -> Result<()> {
        self.umount_and_join()
            .context("FUSE unmount failed; state retained")
    }
}

#[cfg(target_os = "linux")]
fn session(
    core: Arc<FsCore>,
    mountpoint: &Path,
    _volume: &str,
    read_only: bool,
) -> Result<FuseMount> {
    use fuser::MountOption;
    let fs = filesystem::RpoolFs::new(core, mountpoint, read_only)?;
    let mut options = vec![
        MountOption::FSName("rpool".into()),
        MountOption::Subtype("rpool".into()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
        MountOption::NoDev,
        MountOption::NoSuid,
    ];
    options.push(if read_only {
        MountOption::RO
    } else {
        MountOption::RW
    });
    let mut config = Config::default();
    config.mount_options = options;
    config.acl = SessionACL::Owner;
    config.n_threads = Some(4);
    let session = fuser::spawn_mount(fs, mountpoint, &config).context("FUSE mount failed")?;
    Ok(FuseMount { session })
}

#[cfg(target_os = "macos")]
/// How long a new macFUSE session may take to answer the kernel and appear
/// in the mount table.
const MOUNTED_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(target_os = "macos")]
/// macOS: mounts through macFUSE and serves the descriptor on one session
/// thread. The mount counts only once the OS lists it (the kernel mount
/// completes after the session answers INIT); otherwise it is undone and
/// reported as a start failure.
fn session(
    core: Arc<FsCore>,
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<FuseMount> {
    // The mount table lists real paths (`/tmp` is `/private/tmp`). The root
    // of a dead volume still answers stat, so this cannot hang.
    let mountpoint = &mountpoint
        .canonicalize()
        .unwrap_or_else(|_| mountpoint.components().collect());
    macfuse::clear_dead_volume(mountpoint)?;
    let fs = filesystem::RpoolFs::new(core, mountpoint, read_only)?;
    let (fd, backend) = macfuse::mount(mountpoint, volume, read_only)?;
    let mut config = Config::default();
    config.acl = SessionACL::Owner;
    // fuser runs one event loop thread outside Linux.
    config.n_threads = Some(1);
    let session = fuser::Session::from_fd(fs, fd, SessionACL::Owner, config)
        .and_then(fuser::Session::spawn)
        .map_err(|e| anyhow!("macFUSE session failed to start: {e}"))?;
    let deadline = std::time::Instant::now() + MOUNTED_WAIT;
    loop {
        if crate::mount::adapter::macos_mount_source(mountpoint)?.is_some() {
            println!(
                "macFUSE {backend:?} backend serving {}",
                mountpoint.display()
            );
            return Ok(FuseMount {
                session,
                mountpoint: mountpoint.to_path_buf(),
            });
        }
        if session.guard.is_finished() || std::time::Instant::now() >= deadline {
            let ended = session.guard.is_finished();
            let _ = macfuse::unmount(mountpoint);
            let _ = session.join();
            bail!(
                "macFUSE did not complete the mount at {} ({})",
                mountpoint.display(),
                if ended {
                    "the kernel ended the session"
                } else {
                    "no answer within 30 s"
                }
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Mounts the FUSE frontend at `mountpoint`. Called by `frontend::run`.
pub(super) fn mount(
    core: Arc<FsCore>,
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    Ok(Box::new(session(core, mountpoint, volume, read_only)?))
}
