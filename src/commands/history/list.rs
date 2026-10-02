//! `rpool history list`: prints the newest operation-history records.
use crate::history::load_history;
use anyhow::Result;

/// Prints the last `limit` history records, newest first as text or oldest
/// first as JSON, with a shown/total footer. Called via `commands::history::list`.
pub(crate) fn run(limit: usize, json: bool) -> Result<()> {
    let records = load_history()?;
    let start = records.len().saturating_sub(limit);
    let selected = &records[start..];

    if json {
        println!("{}", serde_json::to_string_pretty(selected)?);
        return Ok(());
    }

    for record in selected.iter().rev() {
        println!(
            "{} operation={} status={} started={} finished={} target={}{}",
            record.id,
            record.operation,
            record.status,
            record.started_unix,
            record.finished_unix,
            record.target.as_deref().unwrap_or("-"),
            record
                .message
                .as_ref()
                .map(|message| format!(" message={message}"))
                .unwrap_or_default(),
        );
    }
    println!("shown={} total={}", selected.len(), records.len());
    Ok(())
}
