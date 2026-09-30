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
            M::Run { ref pool, ref id, stop_file: Some(ref s), take_over: false } if pool == "p" && id == "m1" && s.ends_with("stop")
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
