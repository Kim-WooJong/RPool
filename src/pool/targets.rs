//! Resolves the list of target remotes for commands that operate on a pool
//! or an explicit `--remote` list (usage, verify, recover, replicate, health).
use crate::pool::{load_pool_store, validate_pool};
use crate::remote_root::apply_remote_roots;
use anyhow::{bail, Result};

/// Returns the remotes (with remote roots applied) for a named pool or the
/// explicit list; `--pool` and `--remote` are mutually exclusive. The pool is
/// validated first. Used by `commands::usage`, manifest ops and provider health.
pub(crate) fn resolve_target_remotes(
    pool_name: Option<&str>,
    explicit: Vec<String>,
) -> Result<Vec<String>> {
    if pool_name.is_some() && !explicit.is_empty() {
        bail!("--pool and --remote cannot be used together");
    }
    if let Some(name) = pool_name {
        let store = load_pool_store()?;
        let pool = store
            .pools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("pool not found: {name}"))?;
        validate_pool(pool)?;
        return apply_remote_roots(pool.remotes.clone());
    }
    apply_remote_roots(explicit)
}
