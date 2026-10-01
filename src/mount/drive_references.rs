//! Read-only collection of everything the pool's drive metadata in the cloud
//! refers to, for the migration cleanup (`pool migrate retire`): every v6
//! event of every metadata generation, handed to the caller as JSON so it
//! can treat any archive id or object address named there as referenced.
//! Nothing is published, registered or uploaded: events are only listed and
//! read.
use crate::prelude::*;

/// Calls `sink(source, value)` for every drive metadata document of `pool`.
/// Any read error is returned (the caller then keeps everything).
pub(crate) fn collect(
    rclone: &str,
    pool: &str,
    policy: &PoolDefinition,
    sink: &mut dyn FnMut(&str, &Value),
) -> Result<()> {
    let generations = crate::pool::browse_generations::discover(rclone, pool, &policy.remotes)?;
    for generation in generations {
        let epoch = generation.epoch.as_deref();
        let label = format!(
            "drive v6 metadata{}",
            epoch
                .map(|e| format!(" (generation {})", &e[..e.len().min(12)]))
                .unwrap_or_default()
        );
        // Includes checkpointed events (no longer in the event folder),
        // so references kept only in a checkpoint still count.
        let events = super::metadata_pool::read_v6(rclone, pool, policy, epoch)
            .with_context(|| format!("{label} could not be read"))?;
        for event in events.values() {
            sink(&label, &serde_json::to_value(event)?);
        }
    }
    Ok(())
}

/// Every `*.json` file under `root` (at most `depth` levels; the local data
/// folders `spool` and `vfs-cache` are skipped) as JSON.
pub(crate) fn json_tree(
    root: &Path,
    label: &str,
    depth: usize,
    sink: &mut dyn FnMut(&str, &Value),
) -> Result<()> {
    for entry in fs::read_dir(root).with_context(|| format!("{} unreadable", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let name = entry.file_name();
            if depth > 0 && !matches!(name.to_str(), Some("spool" | "vfs-cache" | "shard-cache")) {
                json_tree(&path, label, depth - 1, sink)?;
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "json") {
            let bytes = fs::read(&path)?;
            let value: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not JSON", path.display()))?;
            sink(label, &value);
        }
    }
    Ok(())
}
