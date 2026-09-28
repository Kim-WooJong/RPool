use crate::config::integrity_snapshot_path;
use crate::models::{IntegritySnapshot, ScrubReport, INTEGRITY_SNAPSHOT_VERSION};
use crate::utils::{now_unix, read_json, save_json_atomic};
use anyhow::Result;

pub(crate) fn load_integrity_snapshot() -> Result<Option<IntegritySnapshot>> {
    let path = integrity_snapshot_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&path)?))
}

pub(crate) fn save_integrity_snapshot(
    manifest_source: &str,
    report: &ScrubReport,
) -> Result<IntegritySnapshot> {
    let snapshot = IntegritySnapshot {
        version: INTEGRITY_SNAPSHOT_VERSION,
        manifest_source: manifest_source.to_string(),
        archive_id: report.archive_id.clone(),
        mode: report.mode.clone(),
        checked_unix: now_unix(),
        total: report.total,
        healthy: report.healthy,
        missing: report.missing,
        bad_size: report.bad_size,
        corrupt: report.corrupt,
        errors: report.errors,
        degraded_groups: report.degraded_groups,
        unrecoverable_groups: report.unrecoverable_groups,
        groups: report.groups.clone(),
        issues: report
            .shards
            .iter()
            .filter(|shard| shard.status != "healthy")
            .cloned()
            .collect(),
    };
    let path = integrity_snapshot_path()?;
    save_json_atomic(&path, &snapshot)?;
    Ok(snapshot)
}
