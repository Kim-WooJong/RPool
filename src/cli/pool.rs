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
    /// journal. Reads only; no archive data is written.
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
        #[arg(long)]
        json: bool,
    },
    /// Run or resume a migration. Never deletes; originals stay readable.
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
            M::Plan { ref pool, probe: ProbeMode::Full, download_mib_s: Some(d), upload_mib_s: Some(u), measure_speed: false, json: true }
                if pool == "p" && d == 10.0 && u == 5.0
        ));
        assert!(matches!(
            migrate(&["plan", "p", "--measure-speed"]),
            M::Plan {
                probe: ProbeMode::Quick,
                measure_speed: true,
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
        for bad in [
            vec!["rpool", "pool", "migrate", "run", "p"],
            vec!["rpool", "pool", "migrate", "lost", "p"],
            vec!["rpool", "pool", "migrate", "abandon", "p"],
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
