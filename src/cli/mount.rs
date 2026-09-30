use clap::Args;
use std::path::PathBuf;

/// OS filesystem frontend for a virtual drive.
#[derive(
    clap::ValueEnum,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Frontend {
    /// Native where this build and workspace support it, else WebDAV (default).
    #[default]
    Auto,
    /// rclone mount/VFS over RPool's loopback WebDAV server.
    Dav,
    /// Native FUSE frontend (Linux) over the filesystem core; local workspaces only.
    Fuse,
    /// Native WinFsp frontend (Windows) over the filesystem core; local workspaces only.
    Winfsp,
}

impl Frontend {
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dav => "dav",
            Self::Fuse => "fuse",
            Self::Winfsp => "winfsp",
        }
    }
    /// The frontend a mount actually uses. Native frontends serve online
    /// drives in local or pool-sync (v6) mode; v7 history, bounded shared and
    /// shared-root workspaces, replicas and platforms without one use WebDAV.
    pub(crate) fn resolve(self, args: &MountArgs) -> Self {
        let supported = args.virtual_drive
            && !args.pool_retention
            && !args.bounded_shared
            && args.shared_root.is_none();
        match self {
            Self::Auto => match Self::native_here() {
                Some(native) if supported => native,
                _ => Self::Dav,
            },
            other => other,
        }
    }
    /// The native frontend this build can mount on this OS, if any.
    pub(crate) fn native_here() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Fuse)
        } else if cfg!(all(windows, feature = "winfsp")) {
            Some(Self::Winfsp)
        } else {
            None
        }
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct MountArgs {
    /// Filesystem frontend. `auto` (default) uses the native one where available for
    /// online drives in local or pool-sync (v6) mode, else WebDAV. Native frontends make
    /// fsync/close the local durability point.
    #[arg(long, value_enum, default_value_t = Frontend::Auto)]
    pub(crate) frontend: Frontend,
    /// Mount a native frontend read-only (writes fail with a read-only error).
    #[arg(long)]
    pub(crate) native_read_only: bool,
    /// Apply changed pool membership without resetting the selected workspace. Originals/history remain in a sibling backup; current files are verified in a fresh metadata epoch.
    #[arg(long, requires_all = ["virtual_drive", "pool_sync"], conflicts_with_all = ["account_recovery_from", "shared_root", "worker_name", "bounded_shared", "shared_coordinator", "sync_only", "capacity_only", "migrate_excluded", "cleanup_cache", "recover_spool", "retention_report", "apply_retention", "manifests", "mountpoint"])]
    pub(crate) apply_pool_changes: bool,
    /// Copy locally known recoverable files into a NEW differently named pool/workspace, preserving the source. Does not mount.
    #[arg(long, requires_all = ["virtual_drive", "pool_sync"], conflicts_with_all = ["pool_retention", "shared_root", "worker_name", "bounded_shared", "shared_coordinator", "sync_only", "capacity_only", "migrate_excluded", "cleanup_cache", "recover_spool", "retention_report", "apply_retention", "manifests", "mountpoint"])]
    pub(crate) account_recovery_from: Option<PathBuf>,
    /// Explicitly skip this rclone remote alias while reading recovery data (repeatable, alias only).
    #[arg(long, requires = "account_recovery_from")]
    pub(crate) recovery_skip_remote: Vec<String>,
    /// Use verified completed Reprocess replacements. Account recovery can reuse archives; pool transitions create independent destination data.
    #[arg(long, requires = "pool_sync")]
    pub(crate) recovery_reprocess_plan: Option<PathBuf>,
    /// Automatically replicate sync metadata inside the existing pool (new workspace, no coordinator).
    #[arg(long, requires = "virtual_drive", conflicts_with_all = ["shared_root", "worker_name", "bounded_shared", "shared_coordinator", "apply_retention", "retention_report"])]
    pub(crate) pool_sync: bool,
    /// V7 independently owned snapshots with automatic history collection (NEW workspace; legacy data untouched).
    #[arg(long, requires = "pool_sync")]
    pub(crate) pool_retention: bool,
    /// Mount an existing v7 pool for read-only diagnostics: no background sync, upload or GC.
    #[arg(long, requires_all = ["virtual_drive", "pool_sync", "pool_retention"], conflicts_with_all = ["sync_only", "capacity_only", "cleanup_cache", "recover_spool", "retention_report", "apply_retention", "migrate_excluded", "account_recovery_from", "apply_pool_changes", "manifests"])]
    pub(crate) diagnostic_read_only: bool,
    /// Display name in pool-sync conflicts; defaults to a persistent generated PC name.
    #[arg(long, requires = "pool_sync")]
    pub(crate) pool_worker: Option<String>,
    /// Previous versions per file (enforced with --pool-retention; otherwise stored for future use).
    #[arg(long, requires = "pool_sync", value_parser = clap::value_parser!(u32).range(0..=10000))]
    pub(crate) pool_history_limit: Option<u32>,
    /// Opt-in metadata-first virtual drive (requires a NEW empty workspace).
    #[arg(long)]
    pub(crate) virtual_drive: bool,
    /// Bounded shared protocol: latest cloud state wins; requires a NEW virtual workspace.
    #[arg(long, requires_all = ["virtual_drive", "shared_root"], conflicts_with_all = ["apply_retention", "retention_report"])]
    pub(crate) bounded_shared: bool,
    /// Designate this workspace as the ONE shared checkpoint/retention coordinator.
    #[arg(long, requires = "bounded_shared")]
    pub(crate) shared_coordinator: bool,
    /// Previous cloud revisions per path; default 0 keeps latest only. Offline PCs never pin history.
    #[arg(long, default_value_t = 0)]
    pub(crate) shared_keep_previous: usize,
    /// Clean-shard/read-working-space budget in GiB (0 rejects uncached reads); dirty writes are never evicted.
    #[arg(long, default_value_t = 10)]
    pub(crate) cache_gib: u64,
    /// Native VFS cache target in GiB; open/dirty files may temporarily exceed it.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=1048576))]
    pub(crate) vfs_cache_gib: u64,
    /// Ask rclone to preserve this much free disk space (GiB); not a hard reservation.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(0..=1048576))]
    pub(crate) cache_min_free_gib: u64,
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
    #[arg(long, required_unless_present_any = ["sync_only", "capacity_only", "migrate_excluded", "cleanup_cache", "recover_spool", "retention_report", "apply_retention", "account_recovery_from", "apply_pool_changes"])]
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
    /// Request stop; online unmount retains pending data without draining cloud uploads.
    #[arg(long)]
    pub(crate) stop_file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn diagnostic_mount_requires_v7_and_rejects_mutating_actions() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--mountpoint=/mount",
            "--diagnostic-read-only",
        ];
        assert!(crate::cli::Cli::try_parse_from(base).is_err());
        let valid = ["--virtual-drive", "--pool-sync", "--pool-retention"];
        assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain(valid)).is_ok());
        assert!(crate::cli::Cli::try_parse_from(
            base.into_iter().chain(valid).chain(["--sync-only"])
        )
        .is_err());
        assert!(crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(valid)
                .chain(["--manifest=crypt:file"])
        )
        .is_err());
    }

    #[test]
    fn pool_transition_requires_online_sync_and_excludes_other_actions() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--apply-pool-changes",
        ];
        assert!(crate::cli::Cli::try_parse_from(base).is_err());
        let valid = [
            "--virtual-drive",
            "--pool-sync",
            "--pool-retention",
            "--recovery-reprocess-plan=/plan.json",
        ];
        assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain(valid)).is_ok());
        for conflict in [
            "--sync-only",
            "--account-recovery-from=/old",
            "--mountpoint=R:",
            "--cleanup-cache",
        ] {
            assert!(crate::cli::Cli::try_parse_from(
                base.into_iter().chain(valid).chain([conflict])
            )
            .is_err());
        }
    }

    #[test]
    fn bounded_shared_requires_virtual_shared_root_and_exclusive_coordinator_role() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--sync-only",
        ];
        assert!(
            crate::cli::Cli::try_parse_from(base.into_iter().chain(["--bounded-shared"])).is_err()
        );
        assert!(
            crate::cli::Cli::try_parse_from(base.into_iter().chain(["--shared-coordinator"]))
                .is_err()
        );
        let shared = [
            "--virtual-drive",
            "--bounded-shared",
            "--shared-root=crypt:s",
            "--worker-name=pc",
        ];
        assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain(shared)).is_ok());
        assert!(crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(shared)
                .chain(["--shared-coordinator", "--shared-keep-previous=0"])
        )
        .is_ok());
    }
    #[test]
    fn shared_history_defaults_to_latest_only_but_allows_explicit_history() {
        for (extra, expected) in [(vec![], 0), (vec!["--shared-keep-previous=2"], 2)] {
            let cli = crate::cli::Cli::try_parse_from(
                [
                    "rpool",
                    "mount",
                    "--pool=p",
                    "--workspace=/persistent",
                    "--sync-only",
                    "--virtual-drive",
                    "--bounded-shared",
                    "--shared-root=crypt:s",
                    "--worker-name=pc",
                ]
                .into_iter()
                .chain(extra),
            )
            .unwrap();
            let Some(crate::cli::Commands::Mount(args)) = cli.command else {
                panic!("mount expected")
            };
            assert_eq!(args.shared_keep_previous, expected);
        }
    }
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
    fn frontend_defaults_to_auto_and_resolves_by_workspace_mode() {
        use crate::cli::Frontend;
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/w",
            "--mountpoint=/m",
        ];
        let parse = |extra: &[&str]| {
            let cli =
                crate::cli::Cli::try_parse_from(base.iter().copied().chain(extra.iter().copied()))
                    .unwrap();
            let Some(crate::cli::Commands::Mount(args)) = cli.command else {
                panic!("mount command expected");
            };
            args
        };
        let native = Frontend::native_here().unwrap_or(Frontend::Dav);
        let local = parse(&["--virtual-drive"]);
        assert_eq!(local.frontend, Frontend::Auto);
        assert_eq!(local.frontend.resolve(&local), native);
        let pool = parse(&["--virtual-drive", "--pool-sync"]);
        assert_eq!(
            pool.frontend.resolve(&pool),
            native,
            "v6 pool sync is served natively"
        );
        let v7 = parse(&["--virtual-drive", "--pool-sync", "--pool-retention"]);
        assert_eq!(v7.frontend.resolve(&v7), Frontend::Dav);
        let replica = parse(&[]);
        assert_eq!(replica.frontend.resolve(&replica), Frontend::Dav);
        let explicit = parse(&["--virtual-drive", "--frontend=dav"]);
        assert_eq!(explicit.frontend.resolve(&explicit), Frontend::Dav);
        assert!(crate::cli::Cli::try_parse_from(
            base.iter()
                .copied()
                .chain(["--virtual-drive", "--frontend=nfs"])
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

#[cfg(test)]
mod pool_sync_tests {
    use clap::Parser;
    #[test]
    fn private_snapshot_mode_requires_pool_sync() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/new",
            "--sync-only",
            "--virtual-drive",
            "--pool-retention",
        ];
        assert!(crate::cli::Cli::try_parse_from(base).is_err());
        let parsed = crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(["--pool-sync", "--pool-history-limit=0"]),
        )
        .unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount")
        };
        assert!(args.pool_retention);
        assert_eq!(args.pool_history_limit, Some(0));
    }
    #[test]
    fn automatic_pool_mode_needs_no_shared_root_or_coordinator() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/new",
            "--sync-only",
            "--virtual-drive",
            "--pool-sync",
        ];
        let parsed = crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(["--pool-worker=PC A", "--pool-history-limit=10"]),
        )
        .unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount");
        };
        assert!(args.pool_sync);
        assert_eq!(args.pool_history_limit, Some(10));
        assert!(args.shared_root.is_none());
        assert!(!args.shared_coordinator);
        for incompatible in [
            "--bounded-shared",
            "--shared-coordinator",
            "--shared-root=crypt:x",
            "--pool-history-limit=10001",
        ] {
            assert!(
                crate::cli::Cli::try_parse_from(base.into_iter().chain([incompatible])).is_err()
            );
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use clap::Parser;
    #[test]
    fn cache_limits_parse_and_reject_unbounded_native_target() {
        let base = [
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/persistent",
            "--sync-only",
        ];
        let parsed = crate::cli::Cli::try_parse_from(base.into_iter().chain([
            "--vfs-cache-gib=7",
            "--cache-min-free-gib=3",
            "--cache-gib=5",
            "--spool-gib=8",
        ]))
        .unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount expected")
        };
        assert_eq!(
            (
                args.vfs_cache_gib,
                args.cache_min_free_gib,
                args.cache_gib,
                args.spool_gib
            ),
            (7, 3, 5, 8)
        );
        for invalid in [
            "--vfs-cache-gib=0",
            "--vfs-cache-gib=1048577",
            "--cache-min-free-gib=-1",
            "--spool-gib=0",
        ] {
            assert!(crate::cli::Cli::try_parse_from(base.into_iter().chain([invalid])).is_err());
        }
    }
}

#[cfg(test)]
mod account_recovery_tests {
    use clap::Parser;

    #[test]
    fn recovery_requires_explicit_online_destination_and_disallows_mount_or_gc() {
        let base = [
            "rpool",
            "mount",
            "--pool=new-pool",
            "--workspace=/new",
            "--account-recovery-from=/old",
        ];
        assert!(crate::cli::Cli::try_parse_from(base).is_err());
        let modes = ["--virtual-drive", "--pool-sync"];
        let parsed = crate::cli::Cli::try_parse_from(
            base.into_iter()
                .chain(modes)
                .chain(["--recovery-skip-remote=broken-crypt"]),
        )
        .unwrap();
        let Some(crate::cli::Commands::Mount(args)) = parsed.command else {
            panic!("mount")
        };
        assert_eq!(args.account_recovery_from, Some("/old".into()));
        assert_eq!(args.recovery_skip_remote, ["broken-crypt"]);
        assert!(args.mountpoint.is_none());
        for incompatible in [
            "--pool-retention",
            "--sync-only",
            "--cleanup-cache",
            "--capacity-only",
            "--mountpoint=R:",
            "--manifest=old.json",
        ] {
            assert!(
                crate::cli::Cli::try_parse_from(
                    base.into_iter().chain(modes).chain([incompatible])
                )
                .is_err(),
                "{incompatible}"
            );
        }
        assert!(crate::cli::Cli::try_parse_from([
            "rpool",
            "mount",
            "--pool=p",
            "--workspace=/new",
            "--sync-only",
            "--recovery-skip-remote=x"
        ])
        .is_err());
    }
}
