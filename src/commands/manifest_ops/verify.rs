//! `rpool manifest verify`: compares manifest replicas with a reference manifest.
use crate::manifest::{
    load_manifest_with_storage, validate_manifest, verify_manifest_replicas_with_storage,
};
use crate::pool::resolve_target_remotes;
use crate::storage::reader::StorageReader;
use anyhow::{bail, Result};

/// Checks the replica on each target remote against the validated reference
/// manifest, prints one report per remote and fails unless all are healthy.
pub(crate) fn run(
    rclone: &str,
    manifest_source: &str,
    pool: Option<&str>,
    remotes: Vec<String>,
    json: bool,
) -> Result<()> {
    let reader = StorageReader::rclone(rclone);
    let manifest = load_manifest_with_storage(&reader, manifest_source)?;
    validate_manifest(&manifest)?;
    let requested = resolve_target_remotes(pool, remotes)?;
    let reports = verify_manifest_replicas_with_storage(&reader, &manifest, &requested)?;

    let healthy = reports.iter().filter(|report| report.healthy).count();
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        for report in &reports {
            println!(
                "remote={} healthy={} object={} message={}",
                report.remote, report.healthy, report.object, report.message
            );
        }
        println!("healthy_replicas={}/{}", healthy, reports.len());
    }
    if healthy != reports.len() {
        bail!("one or more manifest replicas are missing, invalid, or stale");
    }
    Ok(())
}
