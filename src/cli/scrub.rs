use clap::Args;
use crate::config::constants::{DEFAULT_RETRIES, DEFAULT_WORKERS};

#[derive(Args, Debug)]
pub(crate) struct ScrubArgs {
    /// Local manifest path or rclone path to manifest.json.
    pub(crate) manifest: String,

    /// Check only existence and size instead of streaming BLAKE3.
    #[arg(long)]
    pub(crate) quick: bool,

    /// Repair recoverable missing/corrupt shards after the scrub.
    #[arg(long)]
    pub(crate) repair: bool,

    /// Show repair actions without writing remote data.
    #[arg(long)]
    pub(crate) dry_run: bool,

    #[arg(long, default_value_t = DEFAULT_WORKERS)]
    pub(crate) workers: usize,

    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    pub(crate) retries: u32,

    /// Emit machine-readable JSON for the scrub report.
    #[arg(long)]
    pub(crate) json: bool,
}
