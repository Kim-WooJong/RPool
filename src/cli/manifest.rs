use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct ManifestArgs {
    #[command(subcommand)]
    pub(crate) command: ManifestCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ManifestCommands {
    /// Replicate a validated manifest to its providers or explicit targets.
    Replicate {
        manifest: String,

        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        pool: Option<String>,

        #[arg(long, default_value_t = crate::config::constants::DEFAULT_RETRIES)]
        retries: u32,
    },

    /// Verify that manifest replicas match a reference manifest.
    Verify {
        manifest: String,

        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        pool: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Recover a local manifest from one of the supplied provider replicas.
    Recover {
        archive_id: String,

        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        pool: Option<String>,

        #[arg(long)]
        output: Option<PathBuf>,
    },
}
