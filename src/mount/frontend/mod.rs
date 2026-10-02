//! Native OS frontends over `FsCore`: WinFsp on Windows (M4) and FUSE on Linux
//! (M5) and macOS (macFUSE, FSKit backend first). They replace rclone mount + loopback WebDAV for local virtual-drive
//! workspaces. DAV stays the default frontend.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fuse;
#[cfg_attr(
    not(all(windows, feature = "winfsp")),
    allow(dead_code, reason = "used by the WinFsp frontend; tested everywhere")
)]
mod names;
mod run;
#[cfg(all(windows, feature = "winfsp"))]
mod winfsp;

#[cfg(target_os = "macos")]
pub(crate) use fuse::macfuse::installed as macfuse_installed;
pub(crate) use run::{native_start_failed, run_native, NativeRun};
