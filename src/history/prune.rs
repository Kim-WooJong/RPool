//! Shrinking the task history.

use crate::config::history_path;
use crate::history::load_history;
use anyhow::Result;
use std::fs;
use std::io::Write;

/// Rewrites the history file keeping only the newest `keep` records; returns
/// how many were removed. Used by `rpool history prune`.
pub(crate) fn prune_history(keep: usize) -> Result<usize> {
    let records = load_history()?;
    let removed = records.len().saturating_sub(keep);
    let retained = if keep >= records.len() {
        records
    } else {
        records[records.len() - keep..].to_vec()
    };

    let path = history_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    for record in retained {
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
    }
    Ok(removed)
}
