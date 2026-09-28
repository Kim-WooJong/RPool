use crate::prelude::*;

pub(crate) fn ensure_positive(value: usize, name: &str) -> Result<()> {
    if value == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(())
}
