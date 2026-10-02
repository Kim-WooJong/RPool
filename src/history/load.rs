//! Reading the task history file.

use crate::config::history_path;
use crate::models::TaskRecord;
use anyhow::Result;
use std::fs;

/// Reads all history records; a missing file is empty and damaged lines are
/// skipped with a note on stderr. Used by `history list`, the dashboard and
/// `doctor`.
pub(crate) fn load_history() -> Result<Vec<TaskRecord>> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let mut records = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // History is informational: a damaged line (two processes appending
        // at once, a crash mid-write) is skipped instead of failing every
        // command that reads the history.
        match serde_json::from_str::<TaskRecord>(line) {
            Ok(record) => records.push(record),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("[history] skipped {skipped} damaged history record(s)");
    }
    Ok(records)
}
