use crate::models::Placement;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub(crate) struct PoolArgs {
    #[command(subcommand)]
    pub(crate) command: PoolCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum PoolCommands {
    /// Query capacity using saved or unsaved pool options; no workspace or mount.
    Capacity(PoolCapacityArgs),
    /// Calculate and save a source-preserving reprocessing plan. No remote writes.
    PlanReprocess {
        name: String,
        #[arg(long = "manifest", required = true)]
        manifests: Vec<String>,
        /// Assumed aggregate download throughput in MiB/s, not a measured rate.
        #[arg(long)]
        download_mib_s: Option<f64>,
        /// Assumed aggregate upload throughput in MiB/s, not a measured rate.
        #[arg(long)]
        upload_mib_s: Option<f64>,
    },
    /// Execute a reviewed plan, retaining every original archive.
    Reprocess {
        #[arg(long)]
        plan: std::path::PathBuf,
    },

    /// List configured storage pools.
    List {
        #[arg(long)]
        json: bool,
    },

    /// List the pool's drive (folders, files, plaintext sizes) read-only from
    /// the pool-sync metadata in the cloud; no workspace or mount needed.
    Browse {
        name: String,
        #[arg(long)]
        json: bool,
    },

    /// Show one storage pool.
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },

    /// Create or replace a storage pool.
    Set {
        name: String,

        #[arg(long = "remote", required = true)]
        remotes: Vec<String>,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_SHARD_MIB, value_parser = clap::value_parser!(u64).range(1..=crate::config::constants::MAX_SHARD_MIB))]
        shard_mib: u64,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_WORKERS)]
        workers: usize,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_RETRIES)]
        retries: u32,

        #[arg(long, value_enum, default_value_t = Placement::RoundRobin)]
        placement: Placement,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_DATA_SHARDS)]
        data_shards: usize,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_PARITY_SHARDS)]
        parity_shards: usize,

        /// Provider per-object limit in bytes; encrypted shard objects must fit.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_object_bytes: Option<u64>,

        /// Encrypt shards in RPool (rclone crypt format) and write them to each
        /// crypt remote's base. Applies to put and reprocess; mounts still use rclone crypt.
        #[arg(long)]
        native_crypt: bool,
    },

    /// Remove a storage pool definition. Stored shards are not touched.
    Remove { name: String },

    /// Move stored archives onto the pool's current (saved) policy after
    /// accounts or coding changed: plan, run/resume, status, lost files.
    Migrate(MigrateArgs),

    /// Measure every account of a saved pool (latency, upload, download) and
    /// show which one limits the pool. Writes random test files under
    /// `<remote>/.rpool-speedtest/<run id>/`, reads them back, verifies and
    /// deletes them. Remotes are tested one after another, so each number is
    /// that account alone; the pool estimate combines them with the pool's
    /// coding and placement.
    SpeedTest {
        name: String,
        #[command(flatten)]
        size: SpeedTestSizeArgs,
    },

    /// Checkpoint the pool's drive metadata so new PCs open it from a few
    /// checkpoint objects, and (once enabled) delete records a checkpoint has
    /// covered on every account for the grace period. Thresholds:
    /// `<config dir>/metadata-compaction.json`.
    Compact {
        name: String,
        /// Show what would be checkpointed, marked and deleted; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// One-time opt-in: allow deleting checkpointed records. Older RPool
        /// versions then stop with an error on this pool (never silently show
        /// an incomplete drive); enable only after every PC is upgraded.
        #[arg(long)]
        enable_deletion: bool,
        #[arg(long)]
        json: bool,
    },
}

/// Test volume and output options shared by `pool speed-test` and
/// `provider speed-test`.
#[derive(Args, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpeedTestSizeArgs {
    /// Total MiB written to (and read back from) each remote.
    #[arg(long, value_name = "N", default_value_t = 16, value_parser = clap::value_parser!(u64).range(1..=4096))]
    pub size_mib: u64,
    /// Number of files the size is split into per remote (default: one per
    /// 4 MiB). `--files 1` tests one large file; many files test per-file
    /// overhead. Each file must be at least 4 KiB.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..=4096))]
    pub files: Option<u64>,
    /// After the test, upload shards (the pool's shard size; 64 MiB without
    /// a pool) at 1, 2, 4, … 32 at once per remote and recommend each
    /// account's simultaneous shard uploads (up to 64 shards per remote;
    /// climbing usually stops earlier).
    #[arg(long)]
    pub tune_uploads: bool,
    /// After the test, read shards back at 1, 2, 4, … 32 at once per remote
    /// (from a read set of 4 uploaded shards) and recommend each account's
    /// simultaneous shard downloads.
    #[arg(long)]
    pub tune_downloads: bool,
    /// Print one JSON report instead of the table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub(crate) struct MigrateArgs {
    #[command(subcommand)]
    pub(crate) command: MigrateCommands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ProbeMode {
    /// List each remote once and compare shard sizes.
    Quick,
    /// Hash every shard of the affected archives.
    Full,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MigrateCommands {
    /// Plan a migration to the saved pool policy and publish it to the cloud
    /// journal. Reads only; no archive data is written. Includes the pool's
    /// drive (its visible files) when it has one.
    Plan {
        pool: String,
        #[arg(long, value_enum, default_value_t = ProbeMode::Quick)]
        probe: ProbeMode,
        /// Assumed aggregate download throughput in MiB/s.
        #[arg(long, conflicts_with = "measure_speed")]
        download_mib_s: Option<f64>,
        /// Assumed aggregate upload throughput in MiB/s.
        #[arg(long, conflicts_with = "measure_speed")]
        upload_mib_s: Option<f64>,
        /// Measure speeds with a small temporary object per remote
        /// (written and deleted under .rpool-sync/bench/).
        #[arg(long)]
        measure_speed: bool,
        /// Leave the pool's drive out (archives only).
        #[arg(long)]
        no_drive: bool,
        /// Also move shards of unaffected archives so existing data follows
        /// the pool's current placement and spreads by free ratio (after
        /// adding an account or changing the placement). Needs every
        /// account's quota.
        #[arg(long)]
        rebalance: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run or resume a migration. Never deletes; originals stay readable.
    /// Drive files get their new archives last; the drive itself switches
    /// with `adopt`.
    Run {
        pool: String,
        #[arg(long)]
        id: String,
        /// Stop cleanly between archives when this file exists.
        #[arg(long)]
        stop_file: Option<std::path::PathBuf>,
        /// Take over archives another PC claimed but did not finish (use only
        /// when that PC has stopped; claims otherwise expire after 2 hours).
        #[arg(long)]
        take_over: bool,
        /// Archives migrated at once (1 = one after another). Each archive
        /// still uses the pool's own relocation workers. Clamped to 1..=16;
        /// default: 4, or fewer when fewer archives remain.
        #[arg(long, value_name = "N")]
        parallel: Option<usize>,
    },
    /// Show migrations recorded in the cloud (all, or one).
    Status {
        pool: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the files a migration found unrecoverable.
    Lost {
        pool: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Mark a migration abandoned. Nothing is deleted.
    Abandon {
        pool: String,
        #[arg(long)]
        id: String,
    },
    /// Clean up after a completed migration. Without --confirm (the
    /// default, same as --dry-run) only lists what could be deleted and what
    /// is kept. With --confirm: quarantined items whose grace period elapsed
    /// are deleted after a fresh reference check, then new candidates are
    /// quarantined (recorded only; their objects stay readable).
    Retire {
        pool: String,
        #[arg(long)]
        id: String,
        /// Only report (the default without --confirm).
        #[arg(long, conflicts_with = "confirm")]
        dry_run: bool,
        #[arg(long)]
        confirm: bool,
        /// Which part of a confirmed run to perform.
        #[arg(long, value_enum, default_value_t = RetireStepArg::All)]
        step: RetireStepArg,
        /// Days a new quarantine waits before permanent deletion.
        #[arg(long, default_value_t = crate::migration::retire::model::DEFAULT_GRACE_DAYS,
              value_parser = clap::value_parser!(u64).range(0..=crate::migration::retire::model::MAX_GRACE_DAYS))]
        grace_days: u64,
        /// Also delete the originals' objects on accounts that left the pool
        /// (when still configured and listable).
        #[arg(long)]
        include_removed_accounts: bool,
        /// Re-verify replacements by reading every shard back.
        #[arg(long)]
        full_verify: bool,
        /// Refuse when a step would delete more than this share of the
        /// pool's objects or bytes.
        #[arg(long, default_value_t = crate::migration::retire::model::DEFAULT_MAX_DELETE_PERCENT,
              value_parser = clap::value_parser!(u32).range(1..=100))]
        max_delete_percent: u32,
        /// Refuse when a step would delete more objects than this.
        #[arg(long, default_value_t = crate::migration::retire::model::DEFAULT_MAX_DELETE_OBJECTS,
              value_parser = clap::value_parser!(u64).range(1..))]
        max_delete_objects: u64,
        /// Skip the mass-delete guard (check the dry-run numbers first).
        #[arg(long)]
        force: bool,
        /// A local drive workspace whose metadata also counts as references
        /// (repeatable).
        #[arg(long = "workspace", value_name = "DIR")]
        workspaces: Vec<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Take items out of the cleanup quarantine during the grace period.
    Restore {
        pool: String,
        #[arg(long)]
        id: String,
        /// Archive id of a quarantined item (repeatable).
        #[arg(long = "item", required_unless_present = "all", conflicts_with = "all")]
        items: Vec<String>,
        /// Every quarantined item that is not being deleted.
        #[arg(long)]
        all: bool,
    },
    /// Adopt the migrated drive: publish it as a new drive generation that
    /// every PC opens (also PCs without the old workspace). Catches up files
    /// changed since planning; never deletes the previous generation.
    Adopt {
        pool: String,
        #[arg(long)]
        id: String,
        /// Leave unrecoverable drive files out of the new generation.
        #[arg(long)]
        accept_lost: bool,
        /// Also switch this PC's drive workspace: it is renamed to a sibling
        /// backup (local-only writes exported) and the next mount opens the
        /// new generation.
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
        /// Stop cleanly between files when this file exists.
        #[arg(long)]
        stop_file: Option<std::path::PathBuf>,
        /// Take over files another PC claimed but did not finish.
        #[arg(long)]
        take_over: bool,
        /// Files migrated at once during the catch-up (1..=16).
        #[arg(long, value_name = "N")]
        parallel: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum RetireStepArg {
    /// Delete due items, then quarantine new candidates.
    All,
    /// Only quarantine new candidates.
    Quarantine,
    /// Only delete due items (and resume interrupted deletions).
    Delete,
}

#[derive(Args, Debug)]
pub(crate) struct PoolCapacityArgs {
    /// Saved pool name; omit when supplying --remote for an unsaved draft.
    pub name: Option<String>,
    #[arg(long = "remote")]
    pub remotes: Vec<String>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=crate::config::constants::MAX_SHARD_MIB))]
    pub shard_mib: Option<u64>,
    #[arg(long)]
    pub data_shards: Option<usize>,
    #[arg(long)]
    pub parity_shards: Option<usize>,
    #[arg(long, value_enum)]
    pub placement: Option<Placement>,
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod capacity_tests {
    use clap::Parser;
    #[test]
    fn saved_and_unsaved_capacity_queries_need_no_workspace_or_mount() {
        for args in [
            vec![
                "rpool",
                "pool",
                "capacity",
                "my-pool",
                "--parity-shards=0",
                "--json",
            ],
            vec![
                "rpool",
                "pool",
                "capacity",
                "--remote=a:",
                "--data-shards=2",
                "--parity-shards=1",
                "--placement=resilient",
            ],
            vec![
                "rpool",
                "pool",
                "capacity",
                "--remote=a:",
                "--data-shards=2",
                "--parity-shards=1",
                "--placement=capacity-first",
            ],
        ] {
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            assert!(matches!(
                cli.command,
                Some(crate::cli::Commands::Pool(super::PoolArgs {
                    command: super::PoolCommands::Capacity(_)
                }))
            ));
        }
    }
    #[test]
    fn browse_parses_name_and_json_without_workspace() {
        let cli = crate::cli::Cli::try_parse_from(["rpool", "pool", "browse", "my-pool", "--json"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::cli::Commands::Pool(super::PoolArgs {
                command: super::PoolCommands::Browse { ref name, json: true }
            })) if name == "my-pool"
        ));
        assert!(crate::cli::Cli::try_parse_from(["rpool", "pool", "browse"]).is_err());
    }

    #[test]
    fn speed_test_commands_parse() {
        use crate::cli::{Cli, Commands, ProviderArgs, ProviderCommands};
        let size = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Some(Commands::Pool(super::PoolArgs {
                command: super::PoolCommands::SpeedTest { name, size },
            })) => (Some(name), size),
            Some(Commands::Provider(ProviderArgs {
                command: ProviderCommands::SpeedTest { remotes, size },
            })) => (Some(remotes.join(",")), size),
            other => panic!("unexpected {other:?}"),
        };
        let (name, s) = size(&["rpool", "pool", "speed-test", "p"]);
        assert_eq!(name.as_deref(), Some("p"));
        assert_eq!((s.size_mib, s.files, s.json), (16, None, false));
        let (_, s) = size(&[
            "rpool",
            "pool",
            "speed-test",
            "p",
            "--size-mib",
            "4096",
            "--files",
            "1",
            "--json",
        ]);
        assert_eq!((s.size_mib, s.files, s.json), (4096, Some(1), true));
        let (remotes, s) = size(&[
            "rpool",
            "provider",
            "speed-test",
            "--remote",
            "a:x",
            "--remote",
            "b:",
            "--files",
            "4096",
        ]);
        assert_eq!(remotes.as_deref(), Some("a:x,b:"));
        assert_eq!(s.files, Some(4096));
        for bad in [
            vec!["rpool", "pool", "speed-test"],
            vec!["rpool", "pool", "speed-test", "p", "--size-mib", "0"],
            vec!["rpool", "pool", "speed-test", "p", "--size-mib", "4097"],
            vec!["rpool", "pool", "speed-test", "p", "--files", "0"],
            vec!["rpool", "pool", "speed-test", "p", "--files", "4097"],
            vec!["rpool", "provider", "speed-test"],
        ] {
            assert!(Cli::try_parse_from(&bad).is_err(), "{bad:?}");
        }
    }

    fn migrate(args: &[&str]) -> super::MigrateCommands {
        let mut full = vec!["rpool", "pool", "migrate"];
        full.extend_from_slice(args);
        match crate::cli::Cli::try_parse_from(full).unwrap().command {
            Some(crate::cli::Commands::Pool(super::PoolArgs {
                command: super::PoolCommands::Migrate(super::MigrateArgs { command }),
            })) => command,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn migrate_subcommands_parse() {
        use super::{MigrateCommands as M, ProbeMode};
        assert!(matches!(
            migrate(&["plan", "p", "--probe", "full", "--download-mib-s", "10", "--upload-mib-s", "5", "--json"]),
            M::Plan { ref pool, probe: ProbeMode::Full, download_mib_s: Some(d), upload_mib_s: Some(u), measure_speed: false, json: true, no_drive: false, rebalance: false }
                if pool == "p" && d == 10.0 && u == 5.0
        ));
        assert!(matches!(
            migrate(&["plan", "p", "--measure-speed", "--rebalance"]),
            M::Plan {
                probe: ProbeMode::Quick,
                measure_speed: true,
                rebalance: true,
                json: false,
                download_mib_s: None,
                ..
            }
        ));
        assert!(matches!(
            migrate(&["run", "p", "--id", "m1", "--stop-file", "/tmp/stop"]),
            M::Run { ref pool, ref id, stop_file: Some(ref s), take_over: false, parallel: None } if pool == "p" && id == "m1" && s.ends_with("stop")
        ));
        assert!(matches!(
            migrate(&["run", "p", "--id", "m1", "--parallel", "3"]),
            M::Run {
                parallel: Some(3),
                stop_file: None,
                ..
            }
        ));
        assert!(matches!(
            migrate(&["status", "p"]),
            M::Status {
                id: None,
                json: false,
                ..
            }
        ));
        assert!(matches!(
            migrate(&["status", "p", "--id", "m1", "--json"]),
            M::Status { id: Some(ref id), json: true, .. } if id == "m1"
        ));
        assert!(matches!(
            migrate(&["lost", "p", "--id", "m1", "--json"]),
            M::Lost { ref id, json: true, .. } if id == "m1"
        ));
        assert!(
            matches!(migrate(&["abandon", "p", "--id", "m1"]), M::Abandon { ref id, .. } if id == "m1")
        );
        assert!(matches!(
            migrate(&["retire", "p", "--id", "m1"]),
            M::Retire {
                confirm: false,
                dry_run: false,
                grace_days: 7,
                max_delete_percent: 50,
                force: false,
                step: super::RetireStepArg::All,
                ..
            }
        ));
        assert!(matches!(
            migrate(&["retire", "p", "--id", "m1", "--confirm", "--step", "delete", "--grace-days", "0",
                      "--max-delete-percent", "80", "--max-delete-objects", "5", "--force",
                      "--include-removed-accounts", "--full-verify", "--workspace", "/w1", "--workspace", "/w2", "--json"]),
            M::Retire { confirm: true, step: super::RetireStepArg::Delete, grace_days: 0, max_delete_percent: 80,
                        max_delete_objects: 5, force: true, include_removed_accounts: true, full_verify: true,
                        json: true, ref workspaces, .. } if workspaces.len() == 2
        ));
        assert!(matches!(
            migrate(&["restore", "p", "--id", "m1", "--item", "a", "--item", "b"]),
            M::Restore { ref items, all: false, .. } if items.len() == 2
        ));
        assert!(matches!(
            migrate(&["restore", "p", "--id", "m1", "--all"]),
            M::Restore { all: true, .. }
        ));
        assert!(matches!(
            migrate(&["plan", "p", "--no-drive"]),
            M::Plan { no_drive: true, .. }
        ));
        assert!(matches!(
            migrate(&["plan", "p"]),
            M::Plan {
                no_drive: false,
                ..
            }
        ));
        assert!(matches!(
            migrate(&["adopt", "p", "--id", "m1", "--accept-lost", "--workspace", "/ws"]),
            M::Adopt { accept_lost: true, workspace: Some(_), ref id, .. } if id == "m1"
        ));
        assert!(matches!(
            migrate(&["adopt", "p", "--id", "m1"]),
            M::Adopt {
                accept_lost: false,
                workspace: None,
                ..
            }
        ));
        for bad in [
            vec!["rpool", "pool", "migrate", "run", "p"],
            vec!["rpool", "pool", "migrate", "lost", "p"],
            vec!["rpool", "pool", "migrate", "abandon", "p"],
            vec!["rpool", "pool", "migrate", "retire", "p"],
            vec![
                "rpool",
                "pool",
                "migrate",
                "retire",
                "p",
                "--id",
                "m",
                "--confirm",
                "--dry-run",
            ],
            vec![
                "rpool",
                "pool",
                "migrate",
                "retire",
                "p",
                "--id",
                "m",
                "--grace-days",
                "400",
            ],
            vec![
                "rpool",
                "pool",
                "migrate",
                "retire",
                "p",
                "--id",
                "m",
                "--max-delete-percent",
                "0",
            ],
            vec!["rpool", "pool", "migrate", "restore", "p", "--id", "m"],
            vec![
                "rpool", "pool", "migrate", "restore", "p", "--id", "m", "--all", "--item", "x",
            ],
            vec!["rpool", "pool", "migrate", "plan"],
            vec!["rpool", "pool", "migrate", "plan", "p", "--probe", "deep"],
            vec![
                "rpool",
                "pool",
                "migrate",
                "plan",
                "p",
                "--measure-speed",
                "--upload-mib-s",
                "3",
            ],
        ] {
            assert!(crate::cli::Cli::try_parse_from(bad).is_err());
        }
    }
}
