use crate::manifest::{load_manifest, validate_manifest};
use crate::pool::resolve_target_remotes;
use crate::prelude::*;
use crate::presentation::print_usage_table;
use crate::storage::admin::{collect_quota_reports, BackendAdmin, RcloneAdmin};
use crate::utils::ensure_positive;

pub(crate) fn usage(
    rclone: &str,
    remotes: Vec<String>,
    pool_name: Option<&str>,
    manifest_src: Option<&str>,
    json: bool,
    workers: usize,
) -> Result<()> {
    let admin = RcloneAdmin::inherited(rclone);
    ensure_positive(workers, "workers")?;
    let mut remotes = resolve_target_remotes(pool_name, remotes)?;

    if let Some(manifest_src) = manifest_src {
        let manifest = load_manifest(rclone, manifest_src)?;
        validate_manifest(&manifest)?;
        remotes.extend(manifest.shards.iter().map(|s| s.remote.clone()));
    }

    remotes.sort();
    remotes.dedup();
    remotes = if remotes.is_empty() {
        admin.catalog()?.physical_remotes()?
    } else {
        admin.catalog()?.capacity_remotes(&remotes)?
    };
    if remotes.is_empty() {
        bail!("no physical rclone storage remotes are available for capacity reporting");
    }

    remotes = admin.catalog()?.capacity_remotes(&remotes)?;
    let reports = collect_quota_reports(&admin, &remotes, workers)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        print_usage_table(&reports);
    }
    Ok(())
}
