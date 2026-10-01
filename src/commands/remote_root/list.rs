use crate::remote_root::load_remote_root_store;
use anyhow::Result;

pub(crate) fn run(json: bool) -> Result<()> {
    let store = load_remote_root_store()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&store)?);
        return Ok(());
    }
    if store.roots.is_empty() {
        println!("No per-remote default paths configured.");
        return Ok(());
    }
    println!("{:<28} DEFAULT PATH", "REMOTE");
    println!("{}", "-".repeat(72));
    for (remote, path) in store.roots {
        println!("{:<28} {}", format!("{remote}:"), path);
    }
    Ok(())
}
