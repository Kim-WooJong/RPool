use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct ExportArgs {
    /// Portable artifact root. Writes config/portable-config.json and, when needed, secrets/rclone.age.
    pub(crate) artifact_root: PathBuf,

    /// Explicit rclone.conf path. If omitted, rclone is asked which config file is active.
    #[arg(long)]
    pub(crate) rclone_config: Option<PathBuf>,

    /// age executable name or path.
    #[arg(long, default_value = "age")]
    pub(crate) age: PathBuf,

    /// age recipient used to encrypt crypt secrets. Required only when crypt remotes exist.
    #[arg(long)]
    pub(crate) age_recipient: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct ImportArgs {
    /// Portable artifact root previously produced by `rpool export`.
    pub(crate) artifact_root: PathBuf,

    /// Explicit rclone.conf path. If omitted, rclone is asked which config file is active.
    #[arg(long)]
    pub(crate) rclone_config: Option<PathBuf>,

    /// age executable name or path.
    #[arg(long, default_value = "age")]
    pub(crate) age: PathBuf,

    /// age identity/private-key file. Must be outside the portable artifact root.
    #[arg(long)]
    pub(crate) age_identity: Option<PathBuf>,

    /// Public recipient for transaction snapshots. If omitted it is derived from --age-identity.
    #[arg(long)]
    pub(crate) age_recipient: Option<String>,

    /// age-keygen executable used to derive the public recipient from the identity.
    #[arg(long, default_value = "age-keygen")]
    pub(crate) age_keygen: PathBuf,

    /// Validate the complete package and target configuration without changing local state.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args, Debug)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub(crate) command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ConfigCommands {
    /// Print resolved active settings paths and portable coverage as JSON (no file contents).
    Paths,
    /// Legacy JSON-only portable rpool settings export. Crypt secrets are excluded.
    Export {
        /// Destination JSON bundle.
        output: PathBuf,
    },

    /// Legacy JSON-only import while preserving machine-local state.
    Import {
        /// Source JSON bundle.
        input: PathBuf,

        /// Validate the bundle without changing local configuration.
        #[arg(long)]
        dry_run: bool,
    },
}
