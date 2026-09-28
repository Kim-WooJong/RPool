use crate::inventory::{find_entries, load_inventory};
use crate::presentation::format_bytes;
use anyhow::Result;

pub(crate) fn run(pattern: &str, json: bool) -> Result<()> {
    let store = load_inventory()?;
    let entries = find_entries(&store, pattern);
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    for entry in &entries {
        println!(
            "{} name={} size={} manifest={}",
            entry.archive_id,
            entry.original_name,
            format_bytes(entry.original_size),
            entry.manifest_source,
        );
    }
    println!("matches={}", entries.len());
    Ok(())
}
