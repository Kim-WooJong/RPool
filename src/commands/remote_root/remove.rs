use crate::remote_root::remove_remote_root;
use anyhow::Result;

pub(crate) fn run(remote: &str) -> Result<()> {
    let config = remove_remote_root(remote)?;
    println!("removed={}", remote.trim_end_matches(':'));
    println!("config={}", config.display());
    Ok(())
}
