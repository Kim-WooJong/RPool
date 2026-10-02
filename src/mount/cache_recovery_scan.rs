//! Reads a frozen rclone VFS cache (`vfs/<fs>/…` data, `vfsMeta/<fs>/…`
//! JSON). Parsing is strict: anything unexpected is classified `Unknown` and
//! kept, never imported.
use crate::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Classification of one cached file, used by `cache_recovery` to decide what to import.
pub(crate) enum EntryKind {
    /// Not dirty: a read cache of data the drive already has.
    Clean,
    /// Dirty and every byte in `[0, size)` is cached.
    Dirty,
    /// Dirty but sparse: the missing ranges cannot be reconstructed.
    Incomplete,
    /// Cannot be classified safely (the reason is kept for the report); never imported.
    Unknown(String),
}

#[derive(Debug, Clone)]
/// One file found in the frozen cache, keyed by its drive path.
pub(crate) struct CacheEntry {
    /// Drive path, `/`-separated.
    pub rel: String,
    /// Cached data file, `None` when only metadata exists.
    pub data: Option<PathBuf>,
    /// Bytes to recover: the metadata size for dirty entries, else the data file length.
    pub size: u64,
    /// Classification of the entry.
    pub kind: EntryKind,
}

#[derive(Deserialize)]
/// rclone `vfsMeta` JSON of one file (only the fields RPool needs).
struct Meta {
    #[serde(rename = "Size")]
    /// Logical file size in bytes.
    size: u64,
    #[serde(rename = "Rs")]
    /// Byte ranges present in the data file; `None` means none recorded.
    ranges: Option<Vec<Range>>,
    #[serde(rename = "Dirty")]
    /// File has unsaved (not yet uploaded) changes.
    dirty: bool,
}
#[derive(Deserialize)]
/// One cached byte range of a file.
struct Range {
    #[serde(rename = "Pos")]
    /// Start offset in bytes.
    pos: u64,
    #[serde(rename = "Size")]
    /// Length in bytes.
    size: u64,
}

/// True when the cached ranges cover `[0, size)` without gaps.
fn covers(ranges: &[Range], size: u64) -> bool {
    let mut spans: Vec<(u64, u64)> = ranges
        .iter()
        .map(|r| (r.pos, r.pos.saturating_add(r.size)))
        .collect();
    spans.sort_unstable();
    let mut reached = 0;
    for (start, end) in spans {
        if start > reached {
            break;
        }
        reached = reached.max(end);
    }
    reached >= size
}

/// The cache's entries, or `Err(reason)` when the tree cannot be mapped to
/// drive paths (then the whole directory is kept).
pub(crate) fn scan(dir: &Path) -> Result<std::result::Result<Vec<CacheEntry>, String>> {
    let (data_root, meta_root) = (dir.join("vfs"), dir.join("vfsMeta"));
    let mut roots = BTreeSet::new();
    for base in [&data_root, &meta_root] {
        if base.exists() {
            for child in fs::read_dir(base)? {
                roots.insert(child?.file_name());
            }
        }
    }
    let root = match roots.len() {
        0 => return Ok(Ok(Vec::new())),
        1 => roots.into_iter().next().unwrap(),
        _ => return Ok(Err("several rclone remotes in one cache".into())),
    };
    let name = root.to_string_lossy();
    if !(name.starts_with(":webdav") || name.starts_with("webdav")) {
        return Ok(Err(format!("not a WebDAV mount cache ({name})")));
    }
    let mut rels = BTreeSet::new();
    let mut unknown = BTreeMap::new();
    for base in [data_root.join(&root), meta_root.join(&root)] {
        walk(&base, &base, &mut rels, &mut unknown)?;
    }
    let mut entries = Vec::new();
    for rel in rels {
        let data = data_root.join(&root).join(&rel);
        let meta = meta_root.join(&root).join(&rel);
        let data_len = fs::symlink_metadata(&data).ok().map(|m| m.len());
        let kind = if let Some(reason) = unknown.get(&rel) {
            EntryKind::Unknown(reason.clone())
        } else {
            classify(&meta, data_len)
        };
        let size = match kind {
            EntryKind::Dirty | EntryKind::Incomplete => meta_size(&meta).unwrap_or(0),
            _ => data_len.unwrap_or(0),
        };
        entries.push(CacheEntry {
            rel,
            data: data_len.map(|_| data),
            size,
            kind,
        });
    }
    Ok(Ok(entries))
}

/// Logical size from a metadata file, `None` if unreadable.
fn meta_size(meta: &Path) -> Option<u64> {
    serde_json::from_slice::<Meta>(&fs::read(meta).ok()?)
        .ok()
        .map(|m| m.size)
}

/// Classifies one entry from its metadata file and data file length.
fn classify(meta: &Path, data_len: Option<u64>) -> EntryKind {
    let bytes = match fs::read(meta) {
        Ok(b) => b,
        // rclone rebuilds data without metadata as not dirty, but a crash
        // right after the first write could leave unsaved bytes like this.
        Err(_) if data_len.unwrap_or(0) == 0 => return EntryKind::Clean,
        Err(_) => return EntryKind::Unknown("cached data without metadata".into()),
    };
    let meta: Meta = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(_) => return EntryKind::Unknown("unreadable rclone metadata".into()),
    };
    if !meta.dirty {
        return EntryKind::Clean;
    }
    if meta.size == 0 {
        return EntryKind::Dirty;
    }
    let complete = data_len.is_some_and(|len| len >= meta.size)
        && covers(meta.ranges.as_deref().unwrap_or(&[]), meta.size);
    if complete {
        EntryKind::Dirty
    } else {
        EntryKind::Incomplete
    }
}

/// Collects relative `/`-separated paths below `base`; non-UTF-8 names and non-regular
/// files are marked unknown.
fn walk(
    base: &Path,
    dir: &Path,
    rels: &mut BTreeSet<String>,
    unknown: &mut BTreeMap<String, String>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for child in fs::read_dir(dir)? {
        let path = child?.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        let rel = path.strip_prefix(base)?;
        let Some(rel_str) = rel.to_str() else {
            let lossy = rel.to_string_lossy().replace('\\', "/");
            unknown.insert(lossy.clone(), "name is not UTF-8".into());
            rels.insert(lossy);
            continue;
        };
        let rel_str = rel_str.replace('\\', "/");
        if kind.is_dir() {
            walk(base, &path, rels, unknown)?;
        } else {
            if !kind.is_file() {
                unknown.insert(rel_str.clone(), "not a regular file".into());
            }
            rels.insert(rel_str);
        }
    }
    Ok(())
}
