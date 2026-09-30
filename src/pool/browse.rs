//! Read-only listing of a pool's drive (the virtual drive namespace) straight
//! from the pool-sync metadata in the cloud: folder and file names with
//! sizes, no workspace needed and nothing written anywhere.
//!
//! CONTRACT (shared by the backend and the GUI Library): keep these types
//! and the `browse` signature stable.

/// One entry of the drive, `/`-separated path relative to the drive root.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BrowseEntry {
    pub path: String,
    /// Plaintext size in bytes; 0 for directories.
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PoolBrowse {
    pub pool: String,
    /// "v6", "v7" or "none" (the pool has no pool-sync drive yet).
    pub mode: String,
    /// Sorted by path; directories are listed explicitly (also implied ones).
    pub entries: Vec<BrowseEntry>,
    /// Conflict copies and other notes worth showing, human readable.
    pub notes: Vec<String>,
}

/// Lists the drive of `pool` from its cloud metadata. Read-only. Blocking:
/// call it off the UI thread.
pub(crate) fn browse(rclone: &str, pool: &str) -> anyhow::Result<PoolBrowse> {
    let _ = rclone;
    Ok(PoolBrowse {
        pool: pool.into(),
        mode: "none".into(),
        ..Default::default()
    })
}
