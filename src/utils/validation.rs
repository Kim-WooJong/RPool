//! Validation of numeric CLI arguments.
use crate::prelude::*;

/// Fails with "`name` must be greater than zero" when `value` is 0 (e.g. `--workers`).
/// Used by `put`, `get`, `status` and `scan`.
pub(crate) fn ensure_positive(value: usize, name: &str) -> Result<()> {
    if value == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(())
}
