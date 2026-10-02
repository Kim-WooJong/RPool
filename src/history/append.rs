//! Appending one finished command to the task history (JSON lines file).

use crate::config::history_path;
use crate::models::TaskRecord;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;

/// Appends `record` as one JSON line to the history file, creating it as
/// needed. Called by `application` after every tracked command.
pub(crate) fn append_record(record: &TaskRecord) -> Result<()> {
    let path = history_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // One write of the whole line: several rpool processes append at once
    // (GUI jobs, mounts), and separate writes of the JSON and the newline
    // could interleave into a broken line.
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;
    Ok(())
}
