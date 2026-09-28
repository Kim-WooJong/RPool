use crate::history::prune_history;
use anyhow::Result;

pub(crate) fn run(keep: usize) -> Result<()> {
    let removed = prune_history(keep)?;
    println!("removed={removed}");
    println!("kept_at_most={keep}");
    Ok(())
}
