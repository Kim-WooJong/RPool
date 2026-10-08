//! Arguments of `rpool provider`: account limits, keepalive, crypt remote
//! creation, name encoding, health, speed test and drain. Dispatched in
//! `application::dispatch` to `commands::provider`, `config_sync::provision`
//! and `speedtest`.
use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
/// Arguments of the `rpool provider` group.
pub(crate) struct ProviderArgs {
    #[command(subcommand)]
    /// Selected `provider` subcommand.
    pub(crate) command: ProviderCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool provider`.
pub(crate) enum ProviderCommands {
    /// Daily upload budgets, bandwidth/request limits and inactivity warnings.
    Limits(super::provider_limits::LimitsArgs),
    /// One cheap authenticated call per account, recorded as activity.
    /// Providers decide what counts as activity; this cannot guarantee that
    /// an inactive account is kept.
    Keepalive {
        /// Account or crypt remote (repeat); default: every backing account.
        #[arg(long = "remote")]
        remotes: Vec<String>,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Add missing crypt remotes for base providers without replacing existing keys.
    EnsureEncryption {
        /// Generated password entropy, not cipher key size.
        #[arg(long, default_value_t = 1024)]
        entropy_bits: usize,
        #[arg(long, default_value = "standard")]
        /// rclone crypt `filename_encryption` for new crypt remotes (`standard`, `obfuscate` or `off`).
        filename_encryption: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        /// rclone crypt `directory_name_encryption` for new crypt remotes.
        directory_encryption: bool,
        /// How encrypted names are stored: base32 (rclone default, works
        /// everywhere), base32768 (about a quarter of the characters, for
        /// path-length-limited remotes such as Windows servers or OneDrive)
        /// or base64 (case-sensitive remotes only).
        #[arg(long, default_value = "base32")]
        filename_encoding: String,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Create a new crypt remote with OS-generated keys, never rotate existing keys.
    Encrypt {
        #[arg(long)]
        /// Name of the new crypt remote, without colon.
        name: String,
        /// Existing non-crypt remote name, without colon.
        #[arg(long)]
        provider: String,
        /// Random password entropy; does not change rclone's encryption algorithm.
        #[arg(long, default_value_t = 1024)]
        entropy_bits: usize,
        #[arg(long, default_value = "standard")]
        /// rclone crypt `filename_encryption` (`standard`, `obfuscate` or `off`).
        filename_encryption: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        /// rclone crypt `directory_name_encryption`.
        directory_encryption: bool,
        /// How encrypted names are stored: base32 (rclone default, works
        /// everywhere), base32768 (about a quarter of the characters, for
        /// path-length-limited remotes such as Windows servers or OneDrive)
        /// or base64 (case-sensitive remotes only).
        #[arg(long, default_value = "base32")]
        filename_encoding: String,
    },
    /// Change how an existing crypt remote stores its names
    /// (`filename_encoding`). Keys and stored bytes are kept, but files
    /// written with the old encoding are not listed or readable until it is
    /// switched back, so a remote that already holds files is refused unless
    /// `--existing-files-ok`.
    NameEncoding {
        /// Crypt remote name, without colon.
        #[arg(long)]
        remote: String,
        /// base32, base32768 or base64.
        #[arg(long)]
        encoding: String,
        #[arg(long)]
        /// Allow the change even though the remote already holds files.
        existing_files_ok: bool,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Change where a provider stores RPool data (its default path) and
    /// repoint the crypt remotes that wrap the old folder. A folder that
    /// already holds files is refused unless `--move-existing`, which moves
    /// them into the new (empty) folder first.
    Location {
        /// Base provider name, without colon.
        #[arg(long)]
        remote: String,
        /// New folder on the provider, e.g. /disk2/rpool; empty for its root.
        #[arg(long, allow_hyphen_values = true)]
        path: String,
        #[arg(long)]
        /// Move files already stored at the old folder to the new one.
        move_existing: bool,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Check provider accessibility, latency, and quota information.
    Health {
        /// Select remotes by name/path; accessibility is checked at each remote root.
        #[arg(long = "remote")]
        remotes: Vec<String>,

        #[arg(long)]
        /// Check the remotes of this saved pool.
        pool: Option<String>,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        /// Number of remotes checked concurrently.
        workers: usize,

        #[arg(long)]
        /// Print JSON instead of text.
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
        /// Test volume, file count, tuning and output options.
        size: crate::cli::pool::SpeedTestSizeArgs,
    },

    /// Copy all shards assigned to one provider to another provider and update the manifest.
    Drain {
        /// Local manifest path or rclone path to manifest.json.
        manifest: String,

        #[arg(long)]
        /// Remote whose shards are moved away.
        from: String,

        #[arg(long)]
        /// Remote that receives the shards.
        to: String,

        /// Output manifest path. Required when the input manifest is remote.
        #[arg(long)]
        output: Option<PathBuf>,

        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        /// Number of shard transfers performed concurrently.
        workers: usize,

        #[arg(long, default_value_t = DEFAULT_RETRIES)]
        /// Whole-shard copy attempts.
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
