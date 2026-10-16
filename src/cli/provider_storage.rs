//! `rpool provider storage …`: what RPool keeps on one provider, grouped by
//! owner, and the cleanup of data no configured pool needs any more. Run by
//! `commands::provider::storage`.
use crate::config::constants::DEFAULT_WORKERS;
use clap::{Args, Subcommand};

#[derive(Args, Debug)]
/// Arguments of the `rpool provider storage` group.
pub(crate) struct StorageArgs {
    #[command(subcommand)]
    /// Selected `storage` subcommand.
    pub(crate) command: StorageCommands,
}

#[derive(Subcommand, Debug)]
/// Subcommands of `rpool provider storage`.
pub(crate) enum StorageCommands {
    /// Show everything RPool stores on one crypt remote, grouped by owner
    /// (pool in use, older drive generations, other pools, speed-test
    /// leftovers, …), with size, object count and whether it may be deleted.
    /// Read-only.
    List {
        /// Crypt remote (`name`, `name:` or `name:path`; a bare name uses its
        /// default path).
        #[arg(long)]
        remote: String,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
    /// Delete groups no configured pool needs. Without `--confirm` only a
    /// preview. The first confirmed run marks the data; a confirmed run after
    /// the grace period checks every reference again and deletes what is
    /// still unused. Speed-test leftovers are deleted at once.
    Clean {
        /// Crypt remote (as for `list`).
        #[arg(long)]
        remote: String,
        /// Group id from `list` (repeat for several).
        #[arg(long = "group")]
        groups: Vec<String>,
        /// Also delete older drive generations that no recorded migration
        /// replaced (a PC whose drive was not switched may still use them).
        #[arg(long)]
        include_unconfirmed_generations: bool,
        /// Also delete data of pools that are not configured on this PC.
        #[arg(long)]
        include_foreign_pools: bool,
        /// Mark new data and delete data whose grace period is over.
        #[arg(long)]
        confirm: bool,
        /// Delete even when it exceeds the mass-delete guard (50% of the
        /// provider's objects or bytes, or 10,000 objects per run).
        #[arg(long)]
        force: bool,
        /// Drop every pending mark on this provider instead.
        #[arg(long, conflicts_with_all = ["confirm", "force"])]
        cancel: bool,
        /// Parallel deletions.
        #[arg(long, default_value_t = DEFAULT_WORKERS)]
        workers: usize,
        #[arg(long)]
        /// Print JSON instead of text.
        json: bool,
    },
}
