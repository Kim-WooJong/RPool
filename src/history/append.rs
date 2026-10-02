use crate::config::history_path;
use crate::models::TaskRecord;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;

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
