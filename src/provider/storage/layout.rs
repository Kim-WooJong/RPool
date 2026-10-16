//! Pure parser of one provider listing into units, the smallest things the
//! storage manager keeps or deletes as a whole. Paths are relative to the
//! provider root (the crypt view, so native-crypt pools look the same):
//!
//! ```text
//! <id>/manifest.json, <id>/{data,parity,shards}/*.bin     archive folder
//! .rpool-sync/events-v6/<scope>/…                          drive metadata, first generation
//! .rpool-sync/events-v6/<scope>/epochs/<epoch>/…           drive metadata, later generation
//! .rpool-sync/migrations-v1/<scope>/<migration>/…          migration journal
//! .rpool-sync/bench/<name>.bin                             migration speed probe
//! .rpool-sync/storage-cleanup-v1/…                         this manager's journal
//! .rpool-speedtest/<run>/…                                 provider speed test run
//! ```
//!
//! Anything else (a folder with files an archive never has, unknown dot
//! folders, files at the root) is `Other` and never deleted. Used by
//! `classify` and the live observation.
use crate::prelude::*;

/// Folder of this manager's journal, relative to the provider root.
pub(crate) const JOURNAL_DIR: &str = ".rpool-sync/storage-cleanup-v1";
/// Folder of drive metadata scopes.
const EVENTS: &str = ".rpool-sync/events-v6/";
/// Folder of migration journal scopes.
const MIGRATIONS: &str = ".rpool-sync/migrations-v1/";

/// One listed object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Stored size in bytes.
    pub size: u64,
    /// Modification time (unix seconds) as listed; `None` when unknown.
    pub modified: Option<u64>,
}

/// Recursive listing of one root: path relative to the root -> object.
pub(crate) type Listing = BTreeMap<String, Entry>;

/// What a unit is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnitKind {
    /// An archive folder (manifest replica and/or shards).
    Archive {
        /// Archive id (the folder name).
        id: String,
    },
    /// One drive metadata generation of a pool scope.
    Generation {
        /// Pool-sync scope (64 hex).
        scope: String,
        /// Epoch (64 hex); `None` for the first generation.
        epoch: Option<String>,
    },
    /// One migration journal.
    Migration {
        /// Pool-sync scope (64 hex) of the pool the migration belongs to.
        scope: String,
        /// Migration id.
        id: String,
    },
    /// One provider speed-test run folder.
    SpeedTest {
        /// Run id (folder name).
        run: String,
    },
    /// One migration speed-probe object.
    Bench {
        /// Object name.
        name: String,
    },
    /// This manager's journal.
    Journal,
    /// Anything RPool does not write in this layout.
    Other {
        /// First path component (or the file name at the root).
        top: String,
    },
}

impl UnitKind {
    /// Stable key of the unit, used in the journal and in reports.
    pub(crate) fn key(&self) -> String {
        match self {
            UnitKind::Archive { id } => format!("archive:{id}"),
            UnitKind::Generation { scope, epoch } => {
                format!("generation:{scope}/{}", epoch.as_deref().unwrap_or("first"))
            }
            UnitKind::Migration { scope, id } => format!("migration:{scope}/{id}"),
            UnitKind::SpeedTest { run } => format!("speedtest:{run}"),
            UnitKind::Bench { name } => format!("bench:{name}"),
            UnitKind::Journal => "journal".into(),
            UnitKind::Other { top } => format!("other:{top}"),
        }
    }
}

/// A unit and its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// What the unit is.
    pub kind: UnitKind,
    /// Objects: path relative to the root -> listed object.
    pub objects: BTreeMap<String, Entry>,
}

impl Unit {
    /// Total stored bytes.
    pub(crate) fn bytes(&self) -> u64 {
        self.objects.values().map(|e| e.size).sum()
    }
    /// Newest modification time; `None` when any object's time is unknown.
    pub(crate) fn newest(&self) -> Option<u64> {
        self.objects
            .values()
            .map(|e| e.modified)
            .try_fold(0u64, |max, t| t.map(|t| max.max(t)))
    }
}

/// Whether `text` is 64 lowercase hex digits (scopes and epochs).
pub(crate) fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A path inside an archive folder (`manifest.json` or a shard).
fn archive_file(rest: &str) -> bool {
    if rest == "manifest.json" {
        return true;
    }
    let Some((dir, name)) = rest.split_once('/') else {
        return false;
    };
    matches!(dir, "data" | "parity" | "shards")
        && !name.contains('/')
        && name.len() > 4
        && name.ends_with(".bin")
}

/// The unit of a `.rpool-sync/…` path (`rest` is after `.rpool-sync/`).
fn sync_unit(path: &str) -> UnitKind {
    let other = || UnitKind::Other {
        top: ".rpool-sync".into(),
    };
    if let Some(rest) = path.strip_prefix(EVENTS) {
        let Some((scope, inner)) = rest.split_once('/') else {
            return other();
        };
        if !is_hex64(scope) {
            return other();
        }
        if let Some(epochs) = inner.strip_prefix("epochs/") {
            return match epochs.split_once('/') {
                Some((epoch, file)) if is_hex64(epoch) && !file.is_empty() => {
                    UnitKind::Generation {
                        scope: scope.into(),
                        epoch: Some(epoch.into()),
                    }
                }
                _ => other(),
            };
        }
        return UnitKind::Generation {
            scope: scope.into(),
            epoch: None,
        };
    }
    if let Some(rest) = path.strip_prefix(MIGRATIONS) {
        let mut parts = rest.splitn(3, '/');
        return match (parts.next(), parts.next(), parts.next()) {
            (Some(scope), Some(id), Some(file))
                if is_hex64(scope) && !id.is_empty() && !file.is_empty() =>
            {
                UnitKind::Migration {
                    scope: scope.into(),
                    id: id.into(),
                }
            }
            _ => other(),
        };
    }
    if let Some(name) = path.strip_prefix(".rpool-sync/bench/") {
        if !name.contains('/') && !name.is_empty() {
            return UnitKind::Bench { name: name.into() };
        }
        return other();
    }
    if path.starts_with(&format!("{JOURNAL_DIR}/")) {
        return UnitKind::Journal;
    }
    other()
}

/// Splits a root listing into units. A top-level folder is an archive only
/// when every file in it is one an archive has; otherwise the whole folder
/// is `Other`.
pub(crate) fn parse(listing: &Listing) -> BTreeMap<String, Unit> {
    let mut kinds: BTreeMap<String, UnitKind> = BTreeMap::new();
    let mut foreign_folders: BTreeSet<&str> = BTreeSet::new();
    for path in listing.keys() {
        let kind = if path.starts_with(".rpool-sync/") {
            sync_unit(path)
        } else if let Some(rest) =
            path.strip_prefix(&format!("{}/", crate::speedtest::model::TEST_DIR))
        {
            match rest.split_once('/') {
                Some((run, file)) if !run.is_empty() && !file.is_empty() => {
                    UnitKind::SpeedTest { run: run.into() }
                }
                _ => UnitKind::Other {
                    top: crate::speedtest::model::TEST_DIR.into(),
                },
            }
        } else {
            match path.split_once('/') {
                Some((id, rest)) if !id.starts_with('.') && archive_file(rest) => {
                    UnitKind::Archive { id: id.into() }
                }
                Some((top, _)) => {
                    if !top.starts_with('.') {
                        foreign_folders.insert(top);
                    }
                    UnitKind::Other { top: top.into() }
                }
                None => UnitKind::Other { top: path.clone() },
            }
        };
        kinds.insert(path.clone(), kind);
    }
    let mut units: BTreeMap<String, Unit> = BTreeMap::new();
    for (path, entry) in listing {
        let mut kind = kinds
            .remove(path)
            .unwrap_or(UnitKind::Other { top: path.clone() });
        if let UnitKind::Archive { id } = &kind {
            if foreign_folders.contains(id.as_str()) {
                kind = UnitKind::Other { top: id.clone() };
            }
        }
        units
            .entry(kind.key())
            .or_insert_with(|| Unit {
                kind,
                objects: BTreeMap::new(),
            })
            .objects
            .insert(path.clone(), *entry);
    }
    units
}

/// Parses `rclone lsjson -R` output into path -> (size, modification time);
/// directories are skipped. Objects whose size the backend does not know
/// (`-1`) are kept with size 0: they still exist (and make a folder `Other`).
pub(crate) fn parse_lsjson(bytes: &[u8]) -> Result<Listing> {
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "Path")]
        path: String,
        #[serde(rename = "Size", default)]
        size: i64,
        #[serde(rename = "IsDir", default)]
        is_dir: bool,
        #[serde(rename = "ModTime", default)]
        mod_time: String,
    }
    let items: Vec<Item> = serde_json::from_slice(bytes).context("invalid rclone lsjson output")?;
    Ok(items
        .into_iter()
        .filter(|item| !item.is_dir)
        .map(|item| {
            let modified = crate::pool::browse_generations::parse_rfc3339(&item.mod_time)
                .and_then(|nanos| u64::try_from(nanos / 1_000_000_000).ok());
            (
                item.path,
                Entry {
                    size: u64::try_from(item.size).unwrap_or(0),
                    modified,
                },
            )
        })
        .collect())
}

/// Every folder above `path` (deepest first), e.g. `a/b/c.bin` -> `a/b`, `a`.
pub(crate) fn parents(path: &str) -> impl Iterator<Item = &str> {
    let mut rest = path;
    std::iter::from_fn(move || {
        let (parent, _) = rest.rsplit_once('/')?;
        rest = parent;
        Some(parent)
    })
}
