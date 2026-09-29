//! Windows WinFsp frontend (native mount M4) over `FsCore`, using
//! `winfsp_wrs`. Built only with `--features winfsp` on Windows, because the
//! WinFsp SDK must be installed to link. The volume is case-insensitive and
//! case-preserving; names resolve onto existing entries regardless of case.
mod filesystem;
mod names;
mod opens;
mod status;

use super::run::NativeMount;
use crate::mount::fs_core::FsCore;
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use winfsp_wrs::{filetime_now, u16cstr, FileSystem, Params, U16CString, VolumeParams};

struct WinFspMount {
    fs: FileSystem,
    stopped: Arc<AtomicBool>,
}
impl NativeMount for WinFspMount {
    fn alive(&self) -> bool {
        !self.stopped.load(Ordering::Acquire)
    }
    fn stop(self: Box<Self>) -> Result<()> {
        // Stops the dispatcher, removes the mount point and frees the context.
        self.fs.stop();
        Ok(())
    }
}

pub(super) fn mount(
    core: Arc<FsCore>,
    mountpoint: &Path,
    read_only: bool,
) -> Result<Box<dyn NativeMount>> {
    winfsp_wrs::init().map_err(|e| anyhow!("WinFsp is not installed or cannot load: {e:?}"))?;
    let mut volume = VolumeParams::default();
    volume
        .set_sector_size(512)
        .set_sectors_per_allocation_unit(8)
        .set_max_component_length(255)
        .set_case_sensitive_search(false)
        .set_case_preserved_names(true)
        .set_unicode_on_disk(true)
        .set_persistent_acls(true)
        // Cached writes reach RPool before Cleanup, where the file is sealed.
        .set_flush_and_purge_on_cleanup(true)
        .set_post_cleanup_when_modified_only(false)
        .set_file_info_timeout(1000)
        .set_read_only_volume(read_only)
        .set_volume_creation_time(filetime_now());
    let _ = volume.set_file_system_name(u16cstr!("RPool"));
    let target = U16CString::from_os_str(mountpoint.as_os_str())
        .map_err(|_| anyhow!("mountpoint contains a NUL character"))?;
    let stopped = Arc::new(AtomicBool::new(false));
    let context = filesystem::RpoolWinFs::new(core, read_only, stopped.clone())?;
    let fs = FileSystem::start(
        Params {
            volume_params: volume,
            ..Default::default()
        },
        Some(&target),
        context,
    )
    .map_err(|status| anyhow!("WinFsp mount failed: NTSTATUS {status:#x}"))?;
    Ok(Box::new(WinFspMount { fs, stopped }))
}
