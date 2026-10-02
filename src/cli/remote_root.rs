//! Arguments of `rpool remote-root`, consumed by `commands::remote_root`.
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
/// Arguments of the `rpool remote-root` group.
pub(crate) struct RemoteRootArgs {
    #[command(subcommand)]
    /// Selected `remote-root` subcommand.
    pub(crate) command: RemoteRootCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool remote-root` (per-remote default storage paths).
pub(crate) enum RemoteRootCommands {
    /// List configured per-remote default paths.
    List {
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },

    /// Set or replace the default path for one rclone remote.
    Set {
        /// Remote name, with or without a trailing colon.
        remote: String,
        /// Default path, e.g. /data/crypt or rpool.
        path: String,
    },

    /// Remove the default path override for one rclone remote.
    Remove {
        /// Remote name, with or without a trailing colon.
        remote: String,
    },
}
