mod config_sync;
mod doctor;
mod drive;
mod history;
mod inventory;
mod manifest;
mod mount;
mod mount_monitor;
pub(crate) mod pool;
mod provider;
mod provider_limits;
mod remote_root;
mod repair;
mod scrub;

use crate::config::constants::*;
use crate::models::Placement;
use clap::{Parser, Subcommand};
pub(crate) use config_sync::{ConfigArgs, ConfigCommands, ExportArgs, ImportArgs};
pub(crate) use doctor::DoctorArgs;
pub(crate) use drive::{
    DriveArgs, DriveCommands, DriveTarget, RetentionArgs, RetentionCommands, TrashArgs,
    TrashCommands, VersionsArgs, VersionsCommands,
};
pub(crate) use history::{HistoryArgs, HistoryCommands};
pub(crate) use inventory::{InventoryArgs, InventoryCommands};
pub(crate) use manifest::{ManifestArgs, ManifestCommands};
pub(crate) use mount::{Frontend, MountArgs};
pub(crate) use mount_monitor::{parse_monitor, MonitorArgs};
pub(crate) use pool::{PoolArgs, PoolCommands};
pub(crate) use provider::{ProviderArgs, ProviderCommands};
pub(crate) use provider_limits::LimitsCommands;
pub(crate) use remote_root::{RemoteRootArgs, RemoteRootCommands};
pub(crate) use repair::RepairArgs;
pub(crate) use scrub::ScrubArgs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "rpool",
    version,
    about = "Parallel sharded storage with optional Reed-Solomon erasure coding over rclone remotes"
)]
pub(crate) struct Cli {
    /// rclone executable name or path.
    #[arg(long, global = true, default_value = "rclone")]
    pub(crate) rclone: String,

    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)] // clap subcommand parsed once per process; boxing adds noise only
pub(crate) enum Commands {
    /// Launch the native rpool storage console.
    Gui,

    /// Mount a persistent local-first read/write workspace backed by a pool.
    ///
    /// `rpool mount monitor [--workspace DIR | --pool NAME] [--watch] [--json]
    /// [--history-minutes N]` shows the live network traffic of running mounts.
    Mount(MountArgs),

    /// Split a local file into logical shards and upload them in parallel.
    Put {
        source: PathBuf,

        /// Explicit rclone destination base. Repeat for every provider.
        /// Cannot be combined with --pool.
        #[arg(long = "remote")]
        remotes: Vec<String>,

        /// Use a named storage pool from the persistent pool configuration.
        #[arg(long)]
        pool: Option<String>,

        /// Plaintext data-shard size in MiB. Overrides the pool default.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=crate::config::constants::MAX_SHARD_MIB))]
        shard_mib: Option<u64>,

        /// Number of physical shard transfers performed concurrently. Overrides the pool default.
        #[arg(long)]
        workers: Option<usize>,

        /// Placement strategy across the supplied remotes. Overrides the pool default.
        #[arg(long, value_enum)]
        placement: Option<Placement>,

        /// Whole-shard attempts. Overrides the pool default.
        #[arg(long)]
        retries: Option<u32>,

        /// Reed-Solomon data shards per coding group. Overrides the pool default.
        #[arg(long)]
        data_shards: Option<usize>,

        /// Reed-Solomon parity shards per coding group. Overrides the pool default.
        #[arg(long)]
        parity_shards: Option<usize>,

        /// Optional deterministic archive identifier override.
        #[arg(long)]
        id: Option<String>,
    },

    /// Restore a file from a local or remote manifest. Missing data shards are reconstructed when possible.
    Get {
        /// Local manifest path or an rclone path to manifest.json.
        manifest: String,
        output: PathBuf,

        /// Number of shard transfers performed concurrently.
        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,

        #[arg(long, default_value_t = DEFAULT_RETRIES)]
        retries: u32,
    },

    /// Check that all physical shards exist, optionally validating every BLAKE3 hash.
    Verify {
        /// Local manifest path or an rclone path to manifest.json.
        manifest: String,

        /// Stream every shard and verify BLAKE3. Without this flag only size/existence is checked.
        #[arg(long)]
        full: bool,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,
    },

    /// Show physical shard availability and Reed-Solomon recoverability.
    Status {
        /// Local manifest path or an rclone path to manifest.json.
        manifest: String,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,

        /// Also show provider quota/usage information via `rclone about --json`.
        #[arg(long)]
        usage: bool,
    },

    /// Show total/used/free space for one or more cloud remotes.
    Usage {
        /// Explicit rclone destination base. Repeat for every provider.
        #[arg(long = "remote")]
        remotes: Vec<String>,

        /// Optionally use remotes from a named storage pool.
        #[arg(long)]
        pool: Option<String>,

        /// Optionally derive provider remotes from an rpool manifest.
        #[arg(long)]
        manifest: Option<String>,

        /// Emit machine-readable JSON instead of the aligned table.
        #[arg(long)]
        json: bool,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,
    },

    /// Export portable rpool configuration and encrypted crypt secrets as one artifact tree.
    Export(ExportArgs),

    /// Import a portable artifact tree and restore crypt secrets transactionally.
    Import(ImportArgs),

    /// Legacy JSON-only portable configuration commands.
    Config(ConfigArgs),

    /// Create and inspect reusable storage pools.
    Pool(PoolArgs),

    /// Trash, file versions and rollback of a pool's online drive.
    Drive(DriveArgs),

    /// Verify, replicate, or recover manifest replicas.
    Manifest(ManifestArgs),

    /// Maintain the rebuildable local archive inventory.
    Inventory(InventoryArgs),

    /// Inspect or prune operation history.
    History(HistoryArgs),

    /// Scan all shards for missing data, size errors, and optional BLAKE3 corruption.
    Scrub(ScrubArgs),

    /// Reconstruct and re-upload recoverable bad shards using Reed-Solomon coding.
    Repair(RepairArgs),

    /// Provider health and migration operations.
    Provider(ProviderArgs),

    /// Configure per-remote default paths used for storage and capacity resolution.
    RemoteRoot(RemoteRootArgs),

    /// Run maintenance diagnostics for rclone, pools, and metadata.
    Doctor(DoctorArgs),
}
