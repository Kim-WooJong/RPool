use crate::models::Placement;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub(crate) struct PoolArgs {
    #[command(subcommand)]
    pub(crate) command: PoolCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum PoolCommands {
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

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_SHARD_MIB)]
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
    },

    /// Remove a storage pool definition. Stored shards are not touched.
    Remove { name: String },
}
