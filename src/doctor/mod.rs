//! `rpool doctor`: health checks of the local configuration and rclone, plus
//! the redacted diagnostics bundle.
pub(crate) mod bundle;
/// Core configuration and rclone checks.
mod check;
mod metadata;
pub(crate) mod rclone_version;

pub(crate) use check::{run_checks, run_checks_with, Diagnostic};
pub(crate) use metadata::check_metadata;
