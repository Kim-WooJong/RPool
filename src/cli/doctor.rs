use clap::Args;

#[derive(Args, Debug)]
pub(crate) struct DoctorArgs {
    /// Check local metadata without probing rclone or legacy remote encryption.
    #[arg(long)]
    pub(crate) local_only: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub(crate) json: bool,
}
