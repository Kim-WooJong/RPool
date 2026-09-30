//! Relocation (work package C): rebuild an archive whose coding is unchanged
//! but whose shards sit (partly) on remotes that left the pool, as a NEW
//! archive on the new policy, moving as little as possible. The original
//! archive is never modified or deleted.
use crate::prelude::*;

/// Result of relocating one archive.
#[derive(Debug, Clone)]
pub(crate) struct Relocated {
    /// The verified replacement manifest (new archive id).
    pub manifest: Manifest,
    /// Where its manifest replicas were written, `remote:path/manifest.json`.
    pub manifest_locations: Vec<String>,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
}

/// Rebuilds `source` (a valid manifest of the pool) for `target`. Shards that
/// are readable on remotes still in `target` may be reused; shards on removed
/// or failed remotes are reconstructed (Reed-Solomon) or copied verbatim from
/// a still-readable removed remote, and written to `target` remotes under the
/// placement rules. The replacement is fully read back and verified before
/// returning. `work_dir` holds temporary data.
pub(crate) fn relocate(
    rclone: &str,
    source: &Manifest,
    target: &PoolDefinition,
    new_archive_id: &str,
    work_dir: &Path,
) -> Result<Relocated> {
    let _ = (rclone, source, target, new_archive_id, work_dir);
    bail!("relocation not implemented yet")
}
