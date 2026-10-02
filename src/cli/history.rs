//! Arguments of `rpool history`, consumed by `commands::history`.
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
/// Arguments of the `rpool history` group.
pub(crate) struct HistoryArgs {
    #[command(subcommand)]
    /// Selected `history` subcommand.
    pub(crate) command: HistoryCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool history`.
pub(crate) enum HistoryCommands {
    /// Show recent task history.
    List {
        #[arg(long, default_value_t = 50)]
        /// Maximum number of records shown, newest first.
        limit: usize,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },

    /// Keep only the newest N history records.
    Prune {
        #[arg(long, default_value_t = 500)]
        /// Number of newest records kept; older ones are deleted.
        keep: usize,
    },
}
