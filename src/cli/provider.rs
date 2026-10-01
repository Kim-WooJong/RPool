use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct ProviderArgs {
    #[command(subcommand)]
    pub(crate) command: ProviderCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ProviderCommands {
    /// Add missing crypt remotes for base providers without replacing existing keys.
    EnsureEncryption {
        /// Deprecated compatibility option; ignored. Uses provider remote default path exactly.
        #[arg(long, default_value = "", hide = true)]
        root: String,
        /// Generated password entropy, not cipher key size.
        #[arg(long, default_value_t = 1024)]
        entropy_bits: usize,
        #[arg(long, default_value = "standard")]
        filename_encryption: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        directory_encryption: bool,
        #[arg(long)]
        json: bool,
    },
    /// Create a new crypt remote with OS-generated keys, never rotate existing keys.
    Encrypt {
        #[arg(long)]
        name: String,
        /// Existing non-crypt remote name, without colon.
        #[arg(long)]
        provider: String,
        /// Deprecated compatibility option; ignored. Uses provider remote default path exactly.
        #[arg(long, default_value = "", hide = true)]
        root: String,
        /// Random password entropy; does not change rclone's encryption algorithm.
        #[arg(long, default_value_t = 1024)]
        entropy_bits: usize,
        #[arg(long, default_value = "standard")]
        filename_encryption: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        directory_encryption: bool,
    },
    /// Check provider accessibility, latency, and quota information.
    Health {
        /// Select remotes by name/path; accessibility is checked at each remote root.
        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        pool: Option<String>,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,

        #[arg(long)]
        json: bool,
    },

    /// Measure latency and upload/download speed of each given crypt remote,
    /// one after another, without a pool. Writes random test files under
    /// `<remote>/.rpool-speedtest/<run id>/`, reads them back, verifies and
    /// deletes them.
    SpeedTest {
        /// Crypt remote to test (repeat for several).
        #[arg(long = "remote", required = true)]
        remotes: Vec<String>,
        #[command(flatten)]
        size: crate::cli::pool::SpeedTestSizeArgs,
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
