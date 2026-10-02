//! Arguments of `rpool repair`, consumed by `commands::repair`.
use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};
use clap::Args;

#[derive(Args, Debug)]
/// Options of `rpool repair`: which manifest, how to check and which groups to repair.
pub(crate) struct RepairArgs {
    /// Local manifest path or rclone path to manifest.json.
    pub(crate) manifest: String,

    /// Check only existence and size before repair.
    #[arg(long)]
    pub(crate) quick: bool,

    #[arg(long, default_value_t = DEFAULT_WORKERS)]
    /// Number of shard transfers performed concurrently.
    pub(crate) workers: usize,

    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    /// Whole-shard attempts.
    pub(crate) retries: u32,

    /// Calculate the repair plan without uploading reconstructed shards.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Repair only the selected Reed-Solomon group. Repeat for multiple groups.
    #[arg(long = "group")]
    pub(crate) groups: Vec<u32>,
}
