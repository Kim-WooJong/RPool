use crate::prelude::*;
use crate::pool::resolve_target_remotes;
use crate::provider::check_providers;
use crate::storage::list_crypt_remotes;
use crate::utils::ensure_positive;
use crate::remote_root::apply_remote_roots;

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
        targets = apply_remote_roots(list_crypt_remotes(rclone)?)?;
    }
    if targets.is_empty() {
        bail!("no rclone crypt remotes are configured");
    }

    let reports = check_providers(rclone, &targets, workers)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        println!("{:<34} {:<10} {:>10} {:>8}  {}", "REMOTE", "STATUS", "LATENCY", "USED%", "DETAIL");
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
                if report.accessible { "healthy" } else { "failed" },
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
