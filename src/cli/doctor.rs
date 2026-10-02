//! Arguments of `rpool doctor`, consumed by `commands::doctor`.
use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
/// Options of `rpool doctor`: local-only checks, JSON output, optional diagnostics bundle.
pub(crate) struct DoctorArgs {
    /// Check local metadata without probing rclone or legacy remote encryption.
    #[arg(long)]
    pub(crate) local_only: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
    /// Write a redacted diagnostics bundle (ZIP) to attach to a bug report:
    /// versions, doctor report, `rclone config redacted`, RPool settings
    /// without secrets, mount log tails and monitoring status. Its
    /// manifest.txt lists every file and the redaction rules.
    #[arg(long, value_name = "FILE.zip")]
    pub(crate) bundle: Option<PathBuf>,
}
