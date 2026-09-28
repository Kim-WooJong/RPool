use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct MountArgs {
    /// Pool receiving verified versions of files changed in this workspace.
    #[arg(long)]
    pub(crate) pool: String,
    /// Persistent local replica, catalog and VFS cache. Never use a temporary folder.
    #[arg(long)]
    pub(crate) workspace: PathBuf,
    /// Unused Windows drive letter, or an existing empty Unix mount directory.
    #[arg(long, required_unless_present = "sync_only")]
    pub(crate) mountpoint: Option<PathBuf>,
    /// Explicit existing archives to import; pool membership is not inferred.
    #[arg(long = "manifest")]
    pub(crate) manifests: Vec<String>,
    /// Archive pending local changes once without mounting.
    #[arg(long)]
    pub(crate) sync_only: bool,
    /// Delay between writeback scans; open VFS handles may remain locally cached.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(2..=86400))]
    pub(crate) interval_seconds: u64,
    /// Creating this file requests a graceful unmount and final writeback scan.
    #[arg(long)]
    pub(crate) stop_file: Option<PathBuf>,
}
