use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct MountArgs {
    /// Opt-in metadata-first virtual drive (requires a NEW empty workspace).
    #[arg(long)]
    pub(crate) virtual_drive: bool,
    /// Verified clean-shard cache budget in GiB. Dirty writes are never evicted.
    #[arg(long, default_value_t = 10)]
    pub(crate) cache_gib: u64,
    /// Maximum local virtual write spool in GiB (includes partial writes); growth fails safely at the limit.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) spool_gib: u64,
    /// Trim only verified clean cache; never delete remote history or dirty spool.
    #[arg(long, requires = "virtual_drive", conflicts_with_all = ["capacity_only", "sync_only", "migrate_excluded"])]
    pub(crate) cleanup_cache: bool,
    /// Export recoverable local spool without mounting, uploading or rewriting metadata.
    #[arg(long, requires = "virtual_drive", conflicts_with_all = ["cleanup_cache", "capacity_only", "sync_only", "migrate_excluded"])]
    pub(crate) recover_spool: bool,
    /// Preview tracked obsolete versions; does not upload or delete remote objects.
    #[arg(long, requires = "virtual_drive", conflicts_with_all = ["sync_only", "capacity_only", "cleanup_cache", "recover_spool", "migrate_excluded"])]
    pub(crate) retention_report: bool,
    /// Apply resumable offline retention (unshared virtual workspaces only).
    #[arg(long, requires_all = ["virtual_drive", "exclusive_archive_ownership"], conflicts_with_all = ["retention_report", "sync_only", "capacity_only", "cleanup_cache", "recover_spool", "migrate_excluded", "shared_root"])]
    pub(crate) apply_retention: bool,
    /// Acknowledge no other workspace, exported manifest or reader uses these archives.
    #[arg(long, requires = "apply_retention")]
    pub(crate) exclusive_archive_ownership: bool,
    /// Previous tracked versions to preserve per original path, besides all live/conflict versions.
    #[arg(long, default_value_t = 3)]
    pub(crate) keep_previous: usize,
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
    #[arg(long, required_unless_present_any = ["sync_only", "capacity_only", "migrate_excluded", "cleanup_cache", "recover_spool", "retention_report", "apply_retention"])]
    pub(crate) mountpoint: Option<PathBuf>,
    /// Explicit existing archives to import; pool membership is not inferred.
    #[arg(long = "manifest")]
    pub(crate) manifests: Vec<String>,
    /// Archive pending local changes once without mounting.
    #[arg(long)]
    pub(crate) sync_only: bool,
    /// Refresh capacity and exclusion warnings without mounting or uploading.
    #[arg(long, conflicts_with_all = ["sync_only", "migrate_excluded"])]
    pub(crate) capacity_only: bool,
    /// Move active archive references to quota-known targets; originals remain stored.
    #[arg(long, conflicts_with = "sync_only")]
    pub(crate) migrate_excluded: bool,
    /// Machine-readable capacity status for the GUI, atomically replaced each scan.
    #[arg(long)]
    pub(crate) status_file: Option<PathBuf>,
    /// Declare backing-remote=account-budget; shared quotas must use the same ID.
    #[arg(long)]
    pub(crate) capacity_domain: Vec<String>,
    /// Declare backing-remote=outage-group (independent from capacity identity).
    #[arg(long)]
    pub(crate) failure_domain: Vec<String>,
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
    fn retention_requires_exclusive_unshared_virtual_mode() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--virtual-drive",
        ];
        assert!(
            crate::cli::Cli::try_parse_from(base.into_iter().chain(["--retention-report"])).is_ok()
        );
        assert!(
            crate::cli::Cli::try_parse_from(base.into_iter().chain(["--apply-retention"])).is_err()
        );
        let apply = ["--apply-retention", "--exclusive-archive-ownership"];
        assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain(apply)).is_ok());
        assert!(crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(apply)
                .chain(["--shared-root=crypt:s", "--worker-name=pc"])
        )
        .is_err());
    }
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

#[cfg(test)]
mod capacity_tests {
    use clap::Parser;
    #[test]
    fn maintenance_modes_need_no_mountpoint_and_conflict_with_sync() {
        for mode in ["--capacity-only", "--migrate-excluded"] {
            let base = [
                "rpool",
                "mount",
                "--pool=p",
                "--workspace=/persistent",
                mode,
            ];
            assert!(crate::cli::Cli::try_parse_from(base).is_ok());
            assert!(
                crate::cli::Cli::try_parse_from(base.into_iter().chain(["--sync-only"])).is_err()
            );
        }
        assert!(crate::cli::Cli::try_parse_from([
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--capacity-only",
            "--migrate-excluded"
        ])
        .is_err());
    }
}
