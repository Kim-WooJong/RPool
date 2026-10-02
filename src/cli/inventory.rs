//! Arguments of `rpool inventory`, consumed by `commands::inventory`.
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
/// Arguments of the `rpool inventory` group.
pub(crate) struct InventoryArgs {
    #[command(subcommand)]
    /// Selected `inventory` subcommand.
    pub(crate) command: InventoryCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool inventory` (the rebuildable local archive index).
pub(crate) enum InventoryCommands {
    /// Add or refresh one manifest in the local rebuildable inventory.
    Add {
        /// Path of the `.rpool.json` manifest to add.
        manifest: String,
    },

    /// Rebuild the local inventory from manifests found recursively in a directory.
    Rebuild {
        /// Directory searched recursively for manifests.
        directory: PathBuf,
    },

    /// List indexed archives.
    List {
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },

    /// Find indexed archives by wildcard or substring.
    Find {
        /// Wildcard (`*`, `?`) or substring matched against indexed archives.
        pattern: String,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },

    /// Show one indexed archive by archive id.
    Info {
        /// Archive id to show.
        archive_id: String,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
}
