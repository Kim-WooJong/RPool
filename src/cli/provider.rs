use clap::{Args, Subcommand};
use std::path::PathBuf;
use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};

#[derive(Args, Debug)]
pub(crate) struct ProviderArgs {
    #[command(subcommand)]
    pub(crate) command: ProviderCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ProviderCommands {
    /// Check provider accessibility, latency, and quota information.
    Health {
        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        pool: Option<String>,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,

        #[arg(long)]
        json: bool,
    },

    /// Copy all shards assigned to one provider to another provider and update the manifest.
    Drain {
        /// Local manifest path or rclone path to manifest.json.
        manifest: String,

        #[arg(long)]
        from: String,

        #[arg(long)]
        to: String,

        /// Output manifest path. Required when the input manifest is remote.
        #[arg(long)]
        output: Option<PathBuf>,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,

        #[arg(long, default_value_t = DEFAULT_RETRIES)]
        retries: u32,

        /// Print the migration plan without copying or modifying anything.
        #[arg(long)]
        dry_run: bool,

        /// Delete old shard objects after copy, full verification, manifest update, and replica write.
        #[arg(long)]
        delete_source: bool,

        /// Permit a migration that weakens single-provider failure safety.
        #[arg(long)]
        allow_risky: bool,
    },
}
