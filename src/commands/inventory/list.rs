//! `rpool inventory list`: lists every indexed archive.
use crate::inventory::load_inventory;
use crate::presentation::format_bytes;
use anyhow::Result;

/// Prints the whole inventory as JSON, or one summary line per archive.
pub(crate) fn run(json: bool) -> Result<()> {
    let store = load_inventory()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&store)?);
        return Ok(());
    }

    if store.entries.is_empty() {
        println!("inventory is empty");
        return Ok(());
    }

    for entry in store.entries.values() {
        let coding = entry
            .coding
            .as_ref()
            .map(|coding| format!("{}+{}", coding.data_shards, coding.parity_shards))
            .unwrap_or_else(|| "none".to_string());
        println!(
            "{} name={} size={} coding={} remotes={} manifest={}",
            entry.archive_id,
            entry.original_name,
            format_bytes(entry.original_size),
            coding,
            entry.remotes.len(),
            entry.manifest_source,
        );
    }
    Ok(())
}
