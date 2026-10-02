//! `rpool provider health`: accessibility, latency and quota of each provider root.
use crate::pool::resolve_target_remotes;
use crate::prelude::*;
use crate::provider::check_providers;
use crate::remote_root::remote_name;
use crate::storage::list_crypt_remotes;
use crate::utils::ensure_positive;

/// Provider health checks the configured remote root, not an archive's
/// storage prefix. The same provider can appear at several Pool paths.
fn provider_roots(remotes: Vec<String>) -> Result<Vec<String>> {
    remotes
        .into_iter()
        .map(|remote| Ok(format!("{}:", remote_name(&remote)?)))
        .collect::<Result<BTreeSet<_>>>()
        .map(|roots| roots.into_iter().collect())
}

/// Checks the remote roots of the given remotes or pool (default: every crypt
/// remote), prints a table or JSON and fails if any root is inaccessible.
pub(crate) fn run(
    rclone: &str,
    remotes: Vec<String>,
    pool_name: Option<&str>,
    workers: usize,
    json: bool,
) -> Result<()> {
    ensure_positive(workers, "workers")?;
    let mut targets = resolve_target_remotes(pool_name, remotes)?;
    if targets.is_empty() {
        targets = list_crypt_remotes(rclone)?;
    }
    let targets = provider_roots(targets)?;
    if targets.is_empty() {
        bail!("no rclone crypt remotes are configured");
    }

    let reports = check_providers(rclone, &targets, workers)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        println!(
            "{:<34} {:<10} {:>10} {:>8}  DETAIL",
            "REMOTE", "STATUS", "LATENCY", "USED%"
        );
        println!("{}", "-".repeat(88));
        for report in &reports {
            let used = report
                .quota
                .used_percent
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "n/a".to_string());
            let detail = report
                .error
                .as_deref()
                .or(report.quota.error.as_deref())
                .unwrap_or("");
            println!(
                "{:<34} {:<10} {:>7} ms {:>8}  {}",
                report.remote,
                if report.accessible {
                    "healthy"
                } else {
                    "failed"
                },
                report.latency_ms,
                used,
                detail
            );
        }
    }

    let failures = reports.iter().filter(|report| !report.accessible).count();
    if failures > 0 {
        bail!("provider health check failed for {failures} remote(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_probes_remote_roots_not_saved_archive_paths() {
        assert_eq!(
            provider_roots(vec![
                "Instance_crypt:rpool".into(),
                "Instance_crypt:other".into(),
                "dropbox_1_crypt:".into(),
            ])
            .unwrap(),
            vec!["Instance_crypt:", "dropbox_1_crypt:"]
        );
        assert!(provider_roots(vec![":rpool".into()]).is_err());
    }
}
