//! Provider administration and tool diagnostics, deliberately separate from data I/O.
pub(crate) mod catalog;
mod quota;
use super::error::StorageError;
use super::rclone::RcloneContext;
use super::traits::OperationContext;
use crate::prelude::*;
pub(crate) use catalog::RemoteCatalog;

pub(crate) trait BackendAdmin: Send + Sync {
    fn discover(&self) -> Result<Vec<String>>;
    fn catalog(&self) -> Result<RemoteCatalog>;
    fn probe(&self, remote: &str) -> Result<()>;
    fn quota(&self, remote: &str) -> QuotaReport;
    fn ensure_encrypted(&self, remote: &str) -> Result<()>;
}
pub(crate) trait ToolDiagnostics: Send + Sync {
    fn version(&self) -> Result<String>;
}

pub(crate) struct RcloneAdmin {
    context: RcloneContext,
}
impl RcloneAdmin {
    pub(crate) fn new(context: RcloneContext) -> Self {
        Self { context }
    }
    pub(crate) fn inherited(executable: &str) -> Self {
        Self::new(RcloneContext::inherited(executable))
    }
    fn operation(&self) -> OperationContext {
        OperationContext::with_deadline(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
    }
}
impl BackendAdmin for RcloneAdmin {
    fn discover(&self) -> Result<Vec<String>> {
        let bytes = self.context.capture(&self.operation(), &["listremotes"])?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| StorageError::invalid_input("invalid remote list encoding"))?;
        let mut names: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
    fn catalog(&self) -> Result<RemoteCatalog> {
        RemoteCatalog::parse(&self.context.config_dump(&self.operation())?)
    }
    fn probe(&self, remote: &str) -> Result<()> {
        // Capture is bounded and errors sanitized by the existing process owner.
        self.context.capture(
            &self.operation(),
            &["lsf", "--max-depth", "1", "--", remote],
        )?;
        Ok(())
    }
    fn quota(&self, remote: &str) -> QuotaReport {
        match self
            .context
            .capture(&self.operation(), &["about", "--json", "--", remote])
        {
            Ok(bytes) => quota::parse(remote, &bytes),
            Err(error) => unavailable_quota(remote, error.to_string()),
        }
    }
    fn ensure_encrypted(&self, remote: &str) -> Result<()> {
        Ok(self.context.ensure_crypt(&self.operation(), remote)?)
    }
}
impl ToolDiagnostics for RcloneAdmin {
    fn version(&self) -> Result<String> {
        let bytes = self.context.capture(&self.operation(), &["version"])?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| StorageError::invalid_input("invalid tool version encoding"))?;
        let first = text
            .lines()
            .next()
            .filter(|s| s.starts_with("rclone v"))
            .ok_or_else(|| StorageError::invalid_input("invalid rclone version response"))?;
        Ok(first.chars().take(160).collect())
    }
}
pub(crate) fn unavailable_quota(remote: &str, error: String) -> QuotaReport {
    QuotaReport {
        remote: remote.into(),
        total: None,
        used: None,
        free: None,
        trashed: None,
        other: None,
        used_percent: None,
        error: Some(error),
    }
}
pub(crate) fn collect_quota_reports(
    admin: &dyn BackendAdmin,
    remotes: &[String],
    workers: usize,
) -> Result<Vec<QuotaReport>> {
    if workers == 0 {
        bail!("workers must be greater than zero");
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.min(remotes.len().max(1)))
        .build()?;
    let mut reports = pool.install(|| {
        remotes
            .par_iter()
            .map(|remote| admin.quota(remote))
            .collect::<Vec<_>>()
    });
    reports.sort_by(|a, b| a.remote.cmp(&b.remote));
    Ok(reports)
}

#[cfg(test)]
mod tests;
