use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub(crate) struct RemoteRootArgs {
    #[command(subcommand)]
    pub(crate) command: RemoteRootCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum RemoteRootCommands {
    /// List configured per-remote default paths.
    List {
        #[arg(long)]
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
