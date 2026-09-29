//! Linux FUSE frontend (native mount M5) over `FsCore`, using `fuser` without
//! libfuse. Close (`flush`) and `fsync` are the local durability points.
mod errno;
mod filesystem;
mod inodes;
#[cfg(test)]
mod tests;

use super::run::NativeMount;
use crate::mount::fs_core::FsCore;
use crate::prelude::*;
use fuser::{BackgroundSession, Config, MountOption, SessionACL};

struct FuseMount {
    session: BackgroundSession,
}
impl NativeMount for FuseMount {
    fn alive(&self) -> bool {
        !self.session.guard.is_finished()
    }
    fn stop(self: Box<Self>) -> Result<()> {
        self.session
            .umount_and_join()
            .context("FUSE unmount failed; state retained")
    }
}

fn session(core: Arc<FsCore>, mountpoint: &Path, read_only: bool) -> Result<BackgroundSession> {
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
    fuser::spawn_mount(fs, mountpoint, &config).context("FUSE mount failed")
}

pub(super) fn mount(
    core: Arc<FsCore>,
    mountpoint: &Path,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    Ok(Box::new(FuseMount {
        session: session(core, mountpoint, read_only)?,
    }))
}
