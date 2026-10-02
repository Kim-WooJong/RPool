//! Arguments of `rpool manifest`, consumed by `commands::manifest_ops`.
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
/// Arguments of the `rpool manifest` group.
pub(crate) struct ManifestArgs {
    #[command(subcommand)]
    /// Selected `manifest` subcommand.
    pub(crate) command: ManifestCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool manifest` (manifest replica maintenance).
pub(crate) enum ManifestCommands {
    /// Replicate a validated manifest to its providers or explicit targets.
    Replicate {
        /// Local manifest path or an rclone path to manifest.json.
        manifest: String,

        #[arg(long = "remote")]
        /// Explicit target remote. Repeatable; cannot be combined with --pool.
        remotes: Vec<String>,

        #[arg(long)]
        /// Use the remotes of this saved pool as targets.
        pool: Option<String>,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_RETRIES)]
        /// Upload attempts per manifest replica.
        retries: u32,
    },

    /// Verify that manifest replicas match a reference manifest.
    Verify {
        /// Reference manifest: local path or an rclone path to manifest.json.
        manifest: String,

        #[arg(long = "remote")]
        /// Remote whose replica is checked. Repeatable; cannot be combined with --pool.
        remotes: Vec<String>,

        #[arg(long)]
        /// Check the replicas on the remotes of this saved pool.
        pool: Option<String>,

        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },

    /// Recover a local manifest from one of the supplied provider replicas.
    Recover {
        /// Archive id whose manifest replica is fetched.
        archive_id: String,

        #[arg(long = "remote")]
        /// Remote searched for a replica. Repeatable; cannot be combined with --pool.
        remotes: Vec<String>,

        #[arg(long)]
        /// Search the remotes of this saved pool.
        pool: Option<String>,

        #[arg(long)]
        /// Where the recovered manifest is written (default: `<archive_id>.rpool.json`).
        output: Option<PathBuf>,
    },
}
