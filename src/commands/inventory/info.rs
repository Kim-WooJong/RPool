use crate::inventory::load_inventory;
use crate::presentation::format_bytes;
use anyhow::Result;

pub(crate) fn run(archive_id: &str, json: bool) -> Result<()> {
    let store = load_inventory()?;
    let entry = store
        .entries
        .get(archive_id)
        .ok_or_else(|| anyhow::anyhow!("archive not found in inventory: {archive_id}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(entry)?);
        return Ok(());
    }

    println!("archive_id={}", entry.archive_id);
    println!("name={}", entry.original_name);
    println!("size={} ({})", entry.original_size, format_bytes(entry.original_size));
    println!("created_unix={}", entry.created_unix);
    println!("indexed_unix={}", entry.indexed_unix);
    println!("content_root_blake3={}", entry.content_root_blake3);
    println!("manifest={}", entry.manifest_source);
    if let Some(coding) = &entry.coding {
        println!("coding={}+{} algorithm={}", coding.data_shards, coding.parity_shards, coding.algorithm);
    } else {
        println!("coding=disabled");
    }
    for remote in &entry.remotes {
        println!("remote={remote}");
    }
    Ok(())
}
