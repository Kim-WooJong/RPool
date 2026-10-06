//! Adding accounts to the pool of an existing workspace without a transition.
//!
//! When the pool only gained accounts (every saved remote is still there and
//! native crypt is unchanged), the existing shards stay valid where they are:
//! manifests name their locations. The new accounts only have to become
//! metadata replicas, so the drive generation's metadata is copied to them
//! (`metadata_backfill`) before the workspace binding names them. Removals
//! and crypt changes still need Apply pool changes or a pool migration.
use super::*;

/// True when `current` only adds remotes to `saved` and keeps native crypt.
pub(super) fn only_adds(saved: &PoolDefinition, current: &PoolDefinition) -> Result<bool> {
    let set = |p: &PoolDefinition| -> Result<BTreeSet<String>> {
        Ok(crate::remote_root::apply_remote_roots(p.remotes.clone())?
            .into_iter()
            .collect())
    };
    let (before, after) = (set(saved)?, set(current)?);
    Ok(before.is_subset(&after) && before != after && saved.native_crypt == current.native_crypt)
}

/// Copies the metadata of `old_roots` to every root of `roots` not among
/// them; returns how many objects were written.
pub(super) fn copy_metadata(
    rclone: &str,
    old_roots: &[String],
    roots: &[String],
    native_crypt: bool,
) -> Result<usize> {
    let family = crate::mount::metadata_checkpoint_model::Family::v6();
    let added: Vec<String> = roots
        .iter()
        .filter(|r| !old_roots.contains(r))
        .cloned()
        .collect();
    let old = crate::mount::metadata_pool::replica_dirs(rclone, old_roots, native_crypt, &family)?;
    let new = crate::mount::metadata_pool::replica_dirs(rclone, &added, native_crypt, &family)?;
    let sources: Vec<_> = old.iter().map(|d| d.backfill_view()).collect();
    let mut copied = 0;
    for (root, target) in added.iter().zip(&new) {
        copied += crate::mount::metadata_backfill::backfill(
            &sources,
            &target.backfill_view(),
            &family.gate_id(),
        )
        .with_context(|| format!("new account {root}"))?;
    }
    Ok(copied)
}
