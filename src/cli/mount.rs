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
    /// Shared encrypted rclone directory, identical on every PC (e.g. crypt:teamspace).
    /// Omit with worker-name for a local-only workspace. Sync is eventual, not file locking.
    #[arg(long, requires = "worker_name")]
    pub(crate) shared_root: Option<String>,
    /// Worker display name used in conflict copies, which preserve both versions.
    #[arg(long, requires = "shared_root")]
    pub(crate) worker_name: Option<String>,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn shared_mount_options_require_each_other() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--sync-only",
        ];
        for extra in [
            vec![],
            vec!["--shared-root=crypt:teamspace", "--worker-name=PC One"],
        ] {
            assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain(extra)).is_ok());
        }
        for extra in ["--shared-root=crypt:teamspace", "--worker-name=PC One"] {
            let error = crate::cli::Cli::try_parse_from(base.into_iter().chain([extra]))
                .err()
                .expect("unpaired shared options must fail");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
        }
    }
}
