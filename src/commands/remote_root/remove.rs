//! `rpool remote-root remove`: drops a remote's default path override.
use crate::remote_root::remove_remote_root;
use anyhow::Result;

/// Removes the default path of `remote` (trailing colon optional) and prints the config file.
pub(crate) fn run(remote: &str) -> Result<()> {
    let config = remove_remote_root(remote)?;
    println!("removed={}", remote.trim_end_matches(':'));
    println!("config={}", config.display());
    Ok(())
}
