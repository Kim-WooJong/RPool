//! FUSE frontend (native mount M5) over `FsCore`, using `fuser`. On Linux it
//! mounts without libfuse; on macOS it mounts through macFUSE (FSKit backend
//! first), loaded at runtime (`macfuse`). The last close (`release`) and
//! `fsync` are the local durability points; per-descriptor `flush` does not
//! seal.
mod errno;
mod filesystem;
mod inodes;
#[cfg(target_os = "macos")]
pub(crate) mod macfuse;
#[cfg(test)]
mod tests;

use super::run::NativeMount;
use crate::mount::fs_core::FsCore;
use crate::prelude::*;
use fuser::{BackgroundSession, Config, SessionACL};

pub(super) struct FuseMount {
    session: BackgroundSession,
    #[cfg(target_os = "macos")]
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
fn session(
    core: Arc<FsCore>,
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<FuseMount> {
    let fs = filesystem::RpoolFs::new(core, mountpoint, read_only)?;
    let (fd, backend) = macfuse::mount(mountpoint, volume, read_only)?;
    let mut config = Config::default();
    config.acl = SessionACL::Owner;
    // fuser runs one event loop thread outside Linux.
    config.n_threads = Some(1);
    let started =
        fuser::Session::from_fd(fs, fd, SessionACL::Owner, config).and_then(fuser::Session::spawn);
    match started {
        Ok(session) => {
            println!(
                "macFUSE {backend:?} backend serving {}",
                mountpoint.display()
            );
            Ok(FuseMount {
                session,
                mountpoint: mountpoint.to_path_buf(),
            })
        }
        Err(e) => {
            let _ = macfuse::unmount(mountpoint);
            Err(anyhow!("macFUSE session failed to start: {e}"))
        }
    }
}

pub(super) fn mount(
    core: Arc<FsCore>,
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    Ok(Box::new(session(core, mountpoint, volume, read_only)?))
}
