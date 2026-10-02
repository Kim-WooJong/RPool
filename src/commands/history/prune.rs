//! `rpool history prune`: trims the operation history.
use crate::history::prune_history;
use anyhow::Result;

/// Keeps at most the newest `keep` history records and prints how many were removed.
pub(crate) fn run(keep: usize) -> Result<()> {
    let removed = prune_history(keep)?;
    println!("removed={removed}");
    println!("kept_at_most={keep}");
    Ok(())
}
