use crate::pool::remove_pool;
use anyhow::Result;

pub(crate) fn run(name: &str) -> Result<()> {
    let path = remove_pool(name)?;
    println!("removed={name}");
    println!("config={}", path.display());
    Ok(())
}
