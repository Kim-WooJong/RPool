//! Arguments of `rpool mount` ([`MountArgs`]) and the filesystem frontend choice
//! ([`Frontend`]) with its runtime availability probes (WinFsp, macFUSE).
//! Used by `mount::run` and the GUI mount form.
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
    /// Native FUSE frontend (Linux; macOS with macFUSE) over the filesystem core; local workspaces only.
    Fuse,
    /// Native WinFsp frontend (Windows) over the filesystem core; local workspaces only.
    Winfsp,
}

impl Frontend {
    /// Value accepted by `--frontend` for this variant; used when the GUI builds a
    /// `rpool mount` command line.
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dav => "dav",
            Self::Fuse => "fuse",
            Self::Winfsp => "winfsp",
        }
    }
    /// The frontend a mount actually uses. `Auto` picks the native frontend
    /// where this build has one and it is installed on this PC (WinFsp), else
    /// WebDAV.
    pub(crate) fn resolve(self) -> Self {
        self.resolve_with(true, Self::native_here())
    }
    /// Pure decision behind [`Self::resolve`]: `native` is the native frontend
    /// usable on this PC (built in and installed), if any.
    pub(crate) fn resolve_with(self, supported: bool, native: Option<Self>) -> Self {
        match self {
            Self::Auto => match native {
                Some(native) if supported => native,
                _ => Self::Dav,
            },
            other => other,
        }
    }
    /// The native frontend compiled into this build for this OS, if any.
    /// Windows builds include WinFsp by default (`winfsp` feature); macOS
    /// builds include FUSE, which needs macFUSE at runtime only.
    pub(crate) fn native_built() -> Option<Self> {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            Some(Self::Fuse)
        } else if cfg!(all(windows, feature = "winfsp")) {
            Some(Self::Winfsp)
        } else {
            None
        }
    }
    /// The native frontend this build can mount on this PC right now: built
    /// in and, for WinFsp, installed (its DLL loads).
    pub(crate) fn native_here() -> Option<Self> {
        Self::native_built().filter(|native| native.installed())
    }
    /// Whether the OS component a native frontend needs is present. Always
    /// `true` for frontends without a runtime dependency RPool can probe.
    pub(crate) fn installed(self) -> bool {
        match self {
            Self::Winfsp => winfsp_installed(),
            Self::Fuse => fuse_installed(),
            Self::Auto | Self::Dav => true,
        }
    }
    /// Error for an explicitly chosen frontend that cannot run on this PC.
    pub(crate) fn ensure_available(self) -> anyhow::Result<()> {
        match unavailable_reason_on(
            self,
            Self::native_built(),
            self.installed(),
            cfg!(target_os = "macos"),
        ) {
            Some(reason) => anyhow::bail!("{reason}"),
            None => Ok(()),
        }
    }
}

/// Why `frontend` cannot mount here, given the natively `built` frontend and
/// whether its OS component is `installed`. Pure, so it is tested everywhere.
#[cfg(test)]
pub(crate) fn unavailable_reason(
    frontend: Frontend,
    built: Option<Frontend>,
    installed: bool,
) -> Option<&'static str> {
    unavailable_reason_on(frontend, built, installed, false)
}

/// `unavailable_reason` with the OS made explicit (`macos`: macFUSE hints).
pub(crate) fn unavailable_reason_on(
    frontend: Frontend,
    built: Option<Frontend>,
    installed: bool,
    macos: bool,
) -> Option<&'static str> {
    match frontend {
        Frontend::Auto | Frontend::Dav => None,
        Frontend::Winfsp if built != Some(Frontend::Winfsp) => Some(
            "the WinFsp frontend needs a Windows build with the `winfsp` feature (on by default); use --frontend dav",
        ),
        Frontend::Winfsp if !installed => Some(
            "WinFsp is not installed on this PC (or its DLL cannot load): install WinFsp from https://winfsp.dev/rel/ and restart RPool, or use --frontend dav",
        ),
        Frontend::Fuse if built != Some(Frontend::Fuse) => Some(
            "the FUSE frontend is available on Linux and macOS (macFUSE) only; use --frontend dav",
        ),
        Frontend::Fuse if !installed && macos => Some(
            "macFUSE is not installed on this Mac: install macFUSE from https://macfuse.github.io/ and restart RPool, or use --frontend dav",
        ),
        Frontend::Winfsp | Frontend::Fuse => None,
    }
}

/// OS probe: whether WinFsp's DLL (registry `InstallDir` + `bin\winfsp-*.dll`)
/// loads. The DLL is delay-loaded, so this must succeed before any WinFsp call.
/// Cached for the process: installing WinFsp takes effect after a restart.
#[cfg(all(windows, feature = "winfsp"))]
fn winfsp_installed() -> bool {
    static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOADED.get_or_init(|| winfsp_wrs::init().is_ok())
}
#[cfg(not(all(windows, feature = "winfsp")))]
/// WinFsp is never available in builds without the Windows `winfsp` feature.
fn winfsp_installed() -> bool {
    false
}

/// OS probe: macOS needs macFUSE (bundle + libfuse); Linux FUSE has no
/// runtime component RPool probes. Cached: installing macFUSE takes effect
/// after a restart (the GUI asks every frame).
#[cfg(target_os = "macos")]
fn fuse_installed() -> bool {
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *INSTALLED.get_or_init(crate::mount::macfuse_installed)
}
#[cfg(not(target_os = "macos"))]
fn fuse_installed() -> bool {
    true
}

/// `rpool mount`: the virtual drive. File bytes stay sharded in the pool;
/// sync metadata is replicated automatically inside the pool (v6 pool sync,
/// no coordinator).
#[derive(Args, Debug, Clone)]
pub(crate) struct MountArgs {
    /// Filesystem frontend. `auto` (default) uses the native one where available, else
    /// WebDAV. Native frontends make fsync and the last close the local durability points.
    #[arg(long, value_enum, default_value_t = Frontend::Auto)]
    pub(crate) frontend: Frontend,
    /// Mount a native frontend read-only (writes fail with a read-only error).
    #[arg(long)]
    pub(crate) native_read_only: bool,
    /// Apply changed pool membership without resetting the selected workspace. Originals/history remain in a sibling backup; current files are verified in a fresh metadata epoch.
    #[arg(long, conflicts_with_all = ["account_recovery_from", "sync_only", "capacity_only", "cleanup_cache", "recover_spool", "manifests", "mountpoint"])]
    pub(crate) apply_pool_changes: bool,
    /// Copy locally known recoverable files into a NEW differently named pool/workspace, preserving the source. Does not mount.
    #[arg(long, conflicts_with_all = ["sync_only", "capacity_only", "cleanup_cache", "recover_spool", "manifests", "mountpoint"])]
    pub(crate) account_recovery_from: Option<PathBuf>,
    /// Import files stored with plain rclone from this `remote:path` into the drive, then upload them. The source is only read. Does not mount; rerun to resume.
    #[arg(long, conflicts_with_all = ["account_recovery_from", "apply_pool_changes", "sync_only", "capacity_only", "cleanup_cache", "recover_spool", "manifests", "mountpoint"])]
    pub(crate) import_from: Option<String>,
    /// Drive folder to import into (default: the drive root).
    #[arg(long, requires = "import_from")]
    pub(crate) import_to: Option<String>,
    /// Upload after this many GiB were copied, so the local spool never holds the whole import.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u64).range(1..=1048576), requires = "import_from")]
    pub(crate) import_batch_gib: u64,
    /// When the drive already has a file at the destination path.
    #[arg(long, value_enum, default_value_t = crate::mount::rclone_import::OnConflict::Skip, requires = "import_from")]
    pub(crate) import_conflict: crate::mount::rclone_import::OnConflict,
    /// Explicitly skip this rclone remote alias while reading recovery data (repeatable, alias only).
    #[arg(long, requires = "account_recovery_from")]
    pub(crate) recovery_skip_remote: Vec<String>,
    /// Use verified completed Reprocess replacements. Account recovery can reuse archives; pool transitions create independent destination data.
    #[arg(long)]
    pub(crate) recovery_reprocess_plan: Option<PathBuf>,
    /// Display name in pool-sync conflicts; defaults to a persistent generated PC name.
    #[arg(long)]
    pub(crate) pool_worker: Option<String>,
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
    #[arg(long, conflicts_with_all = ["capacity_only", "sync_only"])]
    pub(crate) cleanup_cache: bool,
    /// Export recoverable local spool without mounting, uploading or rewriting metadata.
    #[arg(long, conflicts_with_all = ["cleanup_cache", "capacity_only", "sync_only"])]
    pub(crate) recover_spool: bool,
    /// Pool receiving verified versions of files changed in this workspace.
    #[arg(long)]
    pub(crate) pool: String,
    /// Persistent local metadata, write spool and cache. Never use a temporary folder.
    #[arg(long)]
    pub(crate) workspace: PathBuf,
    /// Unused Windows drive letter, or an existing empty Unix mount directory.
    #[arg(long, required_unless_present_any = ["sync_only", "capacity_only", "cleanup_cache", "recover_spool", "account_recovery_from", "apply_pool_changes", "import_from"])]
    pub(crate) mountpoint: Option<PathBuf>,
    /// Explicit existing archives to import; pool membership is not inferred.
    #[arg(long = "manifest")]
    pub(crate) manifests: Vec<String>,
    /// Synchronize pending local changes and cloud metadata once without mounting.
    #[arg(long)]
    pub(crate) sync_only: bool,
    /// Refresh capacity and exclusion warnings without mounting or uploading.
    #[arg(long, conflicts_with = "sync_only")]
    pub(crate) capacity_only: bool,
    /// Machine-readable capacity status for the GUI, atomically replaced each scan.
    #[arg(long)]
    pub(crate) status_file: Option<PathBuf>,
    /// Declare backing-remote=account-budget; shared quotas must use the same ID.
    #[arg(long)]
    pub(crate) capacity_domain: Vec<String>,
    /// Declare backing-remote=outage-group (independent from capacity identity).
    #[arg(long)]
    pub(crate) failure_domain: Vec<String>,
    /// Delay between background sync passes.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(2..=86400))]
    pub(crate) interval_seconds: u64,
    /// Files uploaded at the same time. Default: automatic (twice the coding
    /// groups that fit in `--workers`); all files share those shard transfers.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=64))]
    pub(crate) upload_files: Option<u64>,
    /// Shards this PC transfers at once for the drive (uploads and reads),
    /// shared by all files. A per-PC drive option; omitted = the pool's
    /// saved `workers`. Each account's simultaneous shard uploads still cap
    /// its share (`provider limits`).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=256))]
    pub(crate) workers: Option<u64>,
    /// Request stop; online unmount retains pending data without draining cloud uploads.
    #[arg(long)]
    pub(crate) stop_file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<super::MountArgs, clap::Error> {
        let cli = crate::cli::Cli::try_parse_from(
            ["rpool", "mount"].into_iter().chain(args.iter().copied()),
        )?;
        let Some(crate::cli::Commands::Mount(args)) = cli.command else {
            panic!("mount command expected")
        };
        Ok(args)
    }

    #[test]
    fn removed_mode_flags_are_rejected() {
        let base = ["--pool=p", "--workspace=/w", "--mountpoint=/m"];
        assert!(parse(&base).is_ok());
        for removed in [
            "--virtual-drive",
            "--pool-sync",
            "--pool-retention",
            "--pool-history-limit=1",
            "--diagnostic-read-only",
            "--bounded-shared",
            "--shared-coordinator",
            "--shared-keep-previous=1",
            "--shared-root=crypt:s",
            "--worker-name=pc",
            "--retention-report",
            "--apply-retention",
            "--exclusive-archive-ownership",
            "--keep-previous=1",
            "--migrate-excluded",
        ] {
            let args: Vec<_> = base.into_iter().chain([removed]).collect();
            assert!(parse(&args).is_err(), "{removed}");
        }
    }

    #[test]
    fn upload_files_default_to_automatic_and_are_bounded() {
        let base = ["--pool=p", "--workspace=/w", "--sync-only"];
        assert_eq!(parse(&base).unwrap().upload_files, None);
        let set: Vec<_> = base.into_iter().chain(["--upload-files=8"]).collect();
        assert_eq!(parse(&set).unwrap().upload_files, Some(8));
        for invalid in ["--upload-files=0", "--upload-files=65"] {
            let args: Vec<_> = base.into_iter().chain([invalid]).collect();
            assert!(parse(&args).is_err(), "{invalid}");
        }
    }

    #[test]
    fn workers_are_an_optional_drive_option() {
        let base = ["--pool=p", "--workspace=/w", "--sync-only"];
        assert_eq!(parse(&base).unwrap().workers, None, "the pool's value");
        let set: Vec<_> = base.into_iter().chain(["--workers=12"]).collect();
        assert_eq!(parse(&set).unwrap().workers, Some(12));
        for invalid in ["--workers=0", "--workers=257"] {
            let args: Vec<_> = base.into_iter().chain([invalid]).collect();
            assert!(parse(&args).is_err(), "{invalid}");
        }
    }

    #[test]
    fn pool_worker_is_optional() {
        let args = parse(&[
            "--pool=p",
            "--workspace=/w",
            "--sync-only",
            "--pool-worker=PC A",
        ])
        .unwrap();
        assert_eq!(args.pool_worker.as_deref(), Some("PC A"));
        let args = parse(&["--pool=p", "--workspace=/w", "--sync-only"]).unwrap();
        assert!(args.pool_worker.is_none());
    }

    #[test]
    fn pool_transition_excludes_other_actions() {
        let base = [
            "--pool=p",
            "--workspace=/persistent",
            "--apply-pool-changes",
            "--recovery-reprocess-plan=/plan.json",
        ];
        assert!(parse(&base).is_ok());
        for conflict in [
            "--sync-only",
            "--account-recovery-from=/old",
            "--mountpoint=R:",
            "--cleanup-cache",
        ] {
            let args: Vec<_> = base.into_iter().chain([conflict]).collect();
            assert!(parse(&args).is_err(), "{conflict}");
        }
    }

    #[test]
    fn frontend_defaults_to_auto_and_resolves_natively_where_available() {
        use crate::cli::Frontend;
        let base = ["--pool=p", "--workspace=/w", "--mountpoint=/m"];
        let native = Frontend::native_here().unwrap_or(Frontend::Dav);
        let args = parse(&base).unwrap();
        assert_eq!(args.frontend, Frontend::Auto);
        assert_eq!(args.frontend.resolve(), native);
        let explicit: Vec<_> = base.into_iter().chain(["--frontend=dav"]).collect();
        assert_eq!(parse(&explicit).unwrap().frontend.resolve(), Frontend::Dav);
        let invalid: Vec<_> = base.into_iter().chain(["--frontend=nfs"]).collect();
        assert!(parse(&invalid).is_err());
    }

    #[test]
    fn auto_falls_back_to_dav_without_installed_native() {
        use super::{unavailable_reason, Frontend};
        let w = Some(Frontend::Winfsp);
        // Probe true: Auto mounts natively where the workspace allows it.
        assert_eq!(Frontend::Auto.resolve_with(true, w), Frontend::Winfsp);
        assert_eq!(Frontend::Auto.resolve_with(false, w), Frontend::Dav);
        // Probe false (`native_here()` is None): Auto uses WebDAV.
        assert_eq!(Frontend::Auto.resolve_with(true, None), Frontend::Dav);
        // Explicit choices are kept; availability is checked separately.
        assert_eq!(Frontend::Winfsp.resolve_with(true, None), Frontend::Winfsp);
        assert_eq!(Frontend::Dav.resolve_with(true, w), Frontend::Dav);
        assert_eq!(unavailable_reason(Frontend::Winfsp, w, true), None);
        let missing = unavailable_reason(Frontend::Winfsp, w, false).unwrap();
        assert!(missing.contains("install WinFsp") && missing.contains("--frontend dav"));
        assert!(unavailable_reason(Frontend::Winfsp, None, true)
            .unwrap()
            .contains("`winfsp` feature"));
        assert_eq!(unavailable_reason(Frontend::Auto, None, false), None);
        assert_eq!(unavailable_reason(Frontend::Dav, None, false), None);
        assert_eq!(
            unavailable_reason(Frontend::Fuse, Some(Frontend::Fuse), true),
            None
        );
        assert!(unavailable_reason(Frontend::Fuse, None, true).is_some());
    }

    #[test]
    fn macos_fuse_needs_macfuse_and_auto_falls_back() {
        use super::{unavailable_reason_on, Frontend};
        let f = Some(Frontend::Fuse);
        assert_eq!(unavailable_reason_on(Frontend::Fuse, f, true, true), None);
        let missing = unavailable_reason_on(Frontend::Fuse, f, false, true).unwrap();
        assert!(
            missing.contains("https://macfuse.github.io/") && missing.contains("--frontend dav")
        );
        // Linux has no probe: FUSE is reported installed there.
        assert_eq!(unavailable_reason_on(Frontend::Fuse, f, true, false), None);
        // Without macFUSE `native_here()` is None, so Auto uses WebDAV.
        assert_eq!(Frontend::Auto.resolve_with(true, None), Frontend::Dav);
        assert_eq!(Frontend::Auto.resolve_with(true, f), Frontend::Fuse);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert_eq!(Frontend::native_built(), f);
        }
    }

    #[test]
    fn maintenance_modes_need_no_mountpoint_and_conflict_with_sync() {
        let base = ["--pool=p", "--workspace=/persistent"];
        for mode in ["--capacity-only", "--cleanup-cache", "--recover-spool"] {
            let args: Vec<_> = base.into_iter().chain([mode]).collect();
            assert!(parse(&args).is_ok(), "{mode}");
            let args: Vec<_> = args.into_iter().chain(["--sync-only"]).collect();
            assert!(parse(&args).is_err(), "{mode}");
        }
        assert!(parse(&base).is_err(), "mountpoint required to mount");
    }

    #[test]
    fn cache_limits_parse_and_reject_unbounded_native_target() {
        let base = ["--pool=p", "--workspace=/persistent", "--sync-only"];
        let args: Vec<_> = base
            .into_iter()
            .chain([
                "--vfs-cache-gib=7",
                "--cache-min-free-gib=3",
                "--cache-gib=5",
                "--spool-gib=8",
            ])
            .collect();
        let args = parse(&args).unwrap();
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
            let args: Vec<_> = base.into_iter().chain([invalid]).collect();
            assert!(parse(&args).is_err(), "{invalid}");
        }
    }

    #[test]
    fn import_needs_no_mountpoint_and_excludes_mounting() {
        let base = ["--pool=p", "--workspace=/w", "--import-from=old:x"];
        let args = parse(&base).unwrap();
        assert_eq!((args.import_to, args.import_batch_gib), (None, 4));
        for extra in ["--mountpoint=/m", "--sync-only", "--import-batch-gib=0"] {
            let args: Vec<_> = base.into_iter().chain([extra]).collect();
            assert!(parse(&args).is_err(), "{extra}");
        }
    }

    #[test]
    fn recovery_disallows_mount_or_maintenance() {
        let base = [
            "--pool=new-pool",
            "--workspace=/new",
            "--account-recovery-from=/old",
        ];
        let args: Vec<_> = base
            .into_iter()
            .chain(["--recovery-skip-remote=broken-crypt"])
            .collect();
        let args = parse(&args).unwrap();
        assert_eq!(args.account_recovery_from, Some("/old".into()));
        assert_eq!(args.recovery_skip_remote, ["broken-crypt"]);
        assert!(args.mountpoint.is_none());
        for incompatible in [
            "--sync-only",
            "--cleanup-cache",
            "--capacity-only",
            "--mountpoint=R:",
            "--manifest=old.json",
        ] {
            let args: Vec<_> = base.into_iter().chain([incompatible]).collect();
            assert!(parse(&args).is_err(), "{incompatible}");
        }
    }
}
