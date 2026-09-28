use crate::config::history_path;
use crate::models::TaskRecord;
use anyhow::{Context, Result};
use std::fs;

pub(crate) fn load_history() -> Result<Vec<TaskRecord>> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: TaskRecord = serde_json::from_str(line)
            .with_context(|| format!("invalid history record at line {}", index + 1))?;
        records.push(record);
    }
    Ok(records)
}
