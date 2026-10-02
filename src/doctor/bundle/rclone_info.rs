//! The rclone facts a bundle records, behind a trait so tests use fixtures.
use crate::storage::rclone::RcloneContext;
use crate::storage::traits::OperationContext;
use anyhow::{Context, Result};
use std::time::{Duration, Instant};

/// Source of the rclone facts recorded in a bundle. `RcloneCommand` runs the
/// real binary; tests supply fixtures.
pub(crate) trait RcloneInfo {
    /// Full `rclone version` output.
    fn version(&self) -> Result<String>;
    /// `rclone config redacted` (rclone v1.64.0+); never `config dump/show`.
    fn config_redacted(&self) -> Result<String>;
}

/// `RcloneInfo` that runs the configured rclone executable.
pub(crate) struct RcloneCommand {
    /// rclone invocation context (inherited environment and config).
    context: RcloneContext,
}

impl RcloneCommand {
    /// Wrap the rclone executable path; used by `bundle::export`.
    pub(crate) fn new(executable: &str) -> Self {
        Self {
            context: RcloneContext::inherited(executable),
        }
    }

    /// Run `rclone <args>` with a 30 s deadline and return stdout as lossy UTF-8.
    fn text(&self, args: &[&str]) -> Result<String> {
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
        let bytes = self
            .context
            .capture(&ctx, args)
            .with_context(|| format!("rclone {}", args.join(" ")))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl RcloneInfo for RcloneCommand {
    fn version(&self) -> Result<String> {
        self.text(&["version"])
    }
    fn config_redacted(&self) -> Result<String> {
        self.text(&["config", "redacted"])
    }
}
