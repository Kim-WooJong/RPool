//! Arguments of `rpool provider`: account limits, storage manager,
//! keepalive, crypt remote creation, name encoding, health, speed test and
//! drain. Dispatched in
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
    /// What RPool stores on one provider, grouped by owner, and cleanup of
    /// data no configured pool needs.
    Storage(super::provider_storage::StorageArgs),
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
        /// Physical folder on that same connection; omitted = existing default.
        #[arg(long, allow_hyphen_values = true)]
        path: Option<String>,
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
    /// Read-only, bounded namespace inventory; never verifies or converts stored data.
    EncodingInventory(crate::commands::provider::EncodingInventoryArgs),
    /// Change how an existing crypt remote stores its names
    /// (`filename_encoding`). Keys and stored bytes are kept, but files
    /// written with the old encoding are not listed or readable until it is
    /// switched back. Nonempty locations require explicit stopped-PC and no-conversion acknowledgements.
    NameEncoding {
        /// Crypt remote name, without colon.
        #[arg(long)]
        remote: String,
        /// base32, base32768 or base64.
        #[arg(long)]
        encoding: String,
        #[arg(long)]
        /// Legacy opt-in; alone now fails. Recovery requires all four explicit acknowledgements.
        existing_files_ok: bool,
        /// Confirm mounts, uploads and jobs are stopped on EVERY PC (including offline PCs).
        #[arg(long)]
        all_pcs_stopped: bool,
        /// Acknowledge stored names are NOT converted; changing settings can hide old/new data.
        #[arg(long)]
        old_names_not_converted: bool,
        /// Recovery-only override for mixed views, after inspecting encoding-inventory.
        /// Does NOT merge data or restore visibility of both encodings.
        #[arg(long)]
        recovery_override: bool,
        /// Confirm a read-only encoding-inventory report was reviewed; it is NOT integrity proof.
        #[arg(long)]
        inventory_reviewed: bool,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Query or edit a PC-local crypt disk capacity group without starting a mount.
    /// Only direct SFTP/SMB/local crypt locations; affects new jobs/plans only.
    /// Re-declare on other PCs: capacity declarations are not exported.
    LocationCapacity {
        /// Exact crypt name, without colon or path.
        #[arg(long)]
        remote: String,
        /// Explicitly declare a physical disk group; same disk = same group.
        #[arg(long, conflicts_with = "inherit")]
        group: Option<String>,
        /// Remove only the capacity override; inherit the provider capacity.
        #[arg(long, conflicts_with = "group")]
        inherit: bool,
        #[arg(long)]
        json: bool,
    },
    /// Rename one existing crypt remote; keep the old name as a compatibility alias.
    LocationRename {
        #[arg(long)]
        remote: String,
        #[arg(long)]
        name: String,
        /// Confirm mounts, uploads and jobs stopped on every PC.
        #[arg(long)]
        all_pcs_stopped: bool,
        #[arg(long)]
        json: bool,
    },
    /// Copy/check one crypt's encrypted folder, then switch only its backing path.
    LocationPath {
        #[arg(long)]
        remote: String,
        #[arg(long, allow_hyphen_values = true)]
        path: String,
        #[arg(long)]
        copy_existing: bool,
        /// Confirm mounts, uploads and jobs stopped on every PC.
        #[arg(long)]
        all_pcs_stopped: bool,
        #[arg(long)]
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
