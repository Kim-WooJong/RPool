use crate::remote_root::set_remote_root;
use anyhow::Result;

pub(crate) fn run(remote: &str, path: &str) -> Result<()> {
    let config = set_remote_root(remote, path)?;
    println!("remote={}", remote.trim_end_matches(':'));
    println!("path={path}");
    println!("config={}", config.display());
    Ok(())
}
