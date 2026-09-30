use anyhow::Result;

/// `rpool pool browse`: read-only listing of the pool's drive from cloud metadata.
pub(crate) fn run(rclone: &str, name: &str, json: bool) -> Result<()> {
    let result = crate::pool::browse::browse(rclone, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    for entry in &result.entries {
        if entry.is_dir {
            println!("d {}/", entry.path);
        } else {
            println!("{} {}", entry.size, entry.path);
        }
    }
    for note in &result.notes {
        println!("note: {note}");
    }
    eprintln!(
        "pool={} mode={} entries={}",
        result.pool,
        result.mode,
        result.entries.len()
    );
    Ok(())
}
