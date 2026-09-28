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
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}
