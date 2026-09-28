use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct InventoryArgs {
    #[command(subcommand)]
    pub(crate) command: InventoryCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum InventoryCommands {
    /// Add or refresh one manifest in the local rebuildable inventory.
    Add {
        manifest: String,
    },

    /// Rebuild the local inventory from manifests found recursively in a directory.
    Rebuild {
        directory: PathBuf,
    },

    /// List indexed archives.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Find indexed archives by wildcard or substring.
    Find {
        pattern: String,
        #[arg(long)]
        json: bool,
    },

    /// Show one indexed archive by archive id.
    Info {
        archive_id: String,
        #[arg(long)]
        json: bool,
    },
}
