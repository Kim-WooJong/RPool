use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};
use clap::Args;

#[derive(Args, Debug)]
pub(crate) struct RepairArgs {
    /// Local manifest path or rclone path to manifest.json.
    pub(crate) manifest: String,

    /// Check only existence and size before repair.
    #[arg(long)]
    pub(crate) quick: bool,

    #[arg(long, default_value_t = DEFAULT_WORKERS)]
    pub(crate) workers: usize,

    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    pub(crate) retries: u32,

    /// Calculate the repair plan without uploading reconstructed shards.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Repair only the selected Reed-Solomon group. Repeat for multiple groups.
    #[arg(long = "group")]
    pub(crate) groups: Vec<u32>,
}
