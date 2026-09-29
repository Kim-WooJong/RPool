//! Native OS frontends over `FsCore`: WinFsp on Windows (M4) and FUSE on Linux
//! (M5). They replace rclone mount + loopback WebDAV for local virtual-drive
//! workspaces. DAV stays the default frontend.
#[cfg(target_os = "linux")]
mod fuse;
mod run;

pub(crate) use run::{run_native, NativeRun};
