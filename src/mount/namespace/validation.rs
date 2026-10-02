//! Incremental parts of [`Namespace::validate`].
//!
//! Two caches, each keyed by the exact values it checked ([`Shared::is`]):
//!
//! - reference checks (published, bases and committed receipts name known
//!   events) are skipped while the events and that collection are the same
//!   values that passed;
//! - the path checks (case, spelling, file/directory collisions) over the
//!   committed files and explicit directories are summarized in a
//!   [`PathIndex`]. A save then checks only the paths its pending writes add.
//!
//! Soundness of the path fast path: every path rule is a condition on one
//! path or on a pair of paths, so a set of paths that passes has subsets
//! that pass. The visible files are a subset of committed files plus pending
//! writes, so when that union passes (the committed part passed when the
//! index was built; the index finds every pair involving an added path),
//! the visible files pass. If the union does not pass (a pending delete
//! may resolve the pair), the exact full check decides.
use super::shared::Shared;
use super::{valid_path, Projected};
use crate::mount::shared_model::Event;
use crate::prelude::*;
use std::sync::Weak;

type Events = BTreeMap<String, Event>;

type ReferencesKey = (
    Weak<Events>,
    Weak<BTreeSet<String>>,
    Weak<BTreeMap<String, Vec<String>>>,
    Weak<BTreeMap<String, String>>,
);
type PathsKeyed = (Projected, Weak<BTreeSet<String>>, Option<Arc<PathIndex>>);

#[derive(Default, Clone)]
pub(super) struct Checked {
    /// Events with published, bases, committed intents that passed.
    references: Option<ReferencesKey>,
    /// The projection and directories the index summarizes (`None` index:
    /// they do not pass on their own, so every save uses the full check).
    paths: Option<PathsKeyed>,
}

/// Cache cell; clones copy the cached keys (they remain valid for a clone
/// until it writes the keyed collections).
#[derive(Default)]
pub(super) struct CheckCache(Mutex<Checked>);

impl Clone for CheckCache {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.lock().clone()))
    }
}

impl std::fmt::Debug for CheckCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CheckCache")
    }
}

impl CheckCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Checked> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `check` unless these exact collections already passed it.
    pub(super) fn references(
        &self,
        events: &Shared<Events>,
        published: &Shared<BTreeSet<String>>,
        bases: &Shared<BTreeMap<String, Vec<String>>>,
        committed: &Shared<BTreeMap<String, String>>,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let mut cache = self.lock();
        if let Some((e, p, b, c)) = &cache.references {
            if events.is(e) && published.is(p) && bases.is(b) && committed.is(c) {
                return Ok(());
            }
        }
        check()?;
        cache.references = Some((
            events.downgrade(),
            published.downgrade(),
            bases.downgrade(),
            committed.downgrade(),
        ));
        Ok(())
    }

    /// The index of `projected` files and `directories`, built once.
    pub(super) fn index(
        &self,
        projected: &Projected,
        directories: &Shared<BTreeSet<String>>,
    ) -> Option<Arc<PathIndex>> {
        let mut cache = self.lock();
        if let Some((p, d, index)) = &cache.paths {
            if Arc::ptr_eq(p, projected) && directories.is(d) {
                return index.clone();
            }
        }
        let index = PathIndex::build(projected, directories).ok().map(Arc::new);
        cache.paths = Some((projected.clone(), directories.downgrade(), index.clone()));
        index
    }
}

/// The committed files that `projected` shows.
pub(super) fn committed_files(projected: &Projected) -> impl Iterator<Item = &str> {
    projected
        .iter()
        .filter(|(_, r)| r.event.content.is_some())
        .map(|(p, _)| p.as_str())
}

/// The exact path rules over the visible files `live` and `directories`.
pub(super) fn check_paths(live: &BTreeSet<&str>, directories: &BTreeSet<String>) -> Result<()> {
    let mut folded = BTreeSet::new();
    for path in live {
        if !folded.insert(path.to_lowercase()) {
            bail!("case collision in pending namespace");
        }
    }
    let mut spellings = BTreeMap::new();
    for path in live
        .iter()
        .copied()
        .chain(directories.iter().map(String::as_str))
    {
        valid_path(path)?;
        let mut prefix = String::new();
        for part in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if spellings
                .insert(prefix.to_lowercase(), prefix.clone())
                .is_some_and(|old| old != prefix)
            {
                bail!("directory case collision in pending namespace");
            }
        }
        let parts: Vec<_> = path.split('/').collect();
        for i in 1..parts.len() {
            if folded.contains(&parts[..i].join("/").to_lowercase()) {
                bail!("file/directory collision in pending namespace");
            }
        }
    }
    for directory in directories {
        if folded.contains(&directory.to_lowercase()) {
            bail!("directory/file collision");
        }
    }
    Ok(())
}

/// The proper prefixes of `path` (`a`, `a/b` for `a/b/c`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(at, _)| &path[..at])
}

/// Committed files and directories that pass [`check_paths`], summarized
/// for checking added paths against them.
pub(super) struct PathIndex {
    /// Folded file path to its spelling.
    files: BTreeMap<String, String>,
    /// Folded prefix (every path and its ancestors) to its spelling.
    spellings: BTreeMap<String, String>,
    /// Folded proper prefixes of every path.
    ancestors: BTreeSet<String>,
    /// Folded directories.
    directories: BTreeSet<String>,
}

impl PathIndex {
    fn build(projected: &Projected, directories: &BTreeSet<String>) -> Result<Self> {
        let live: BTreeSet<&str> = committed_files(projected).collect();
        check_paths(&live, directories)?;
        let mut index = Self {
            files: live
                .iter()
                .map(|p| (p.to_lowercase(), p.to_string()))
                .collect(),
            spellings: BTreeMap::new(),
            ancestors: BTreeSet::new(),
            directories: directories.iter().map(|d| d.to_lowercase()).collect(),
        };
        for path in live
            .iter()
            .copied()
            .chain(directories.iter().map(String::as_str))
        {
            for prefix in ancestors(path) {
                index.ancestors.insert(prefix.to_lowercase());
                index
                    .spellings
                    .insert(prefix.to_lowercase(), prefix.to_string());
            }
            index
                .spellings
                .insert(path.to_lowercase(), path.to_string());
        }
        Ok(index)
    }

    /// Whether the indexed paths plus the files `added` (each already a
    /// valid path) pass [`check_paths`]. `false` is not an error: the
    /// caller then runs the exact check.
    pub(super) fn admits(&self, added: &BTreeSet<&str>) -> bool {
        let mut files: BTreeMap<String, &str> = BTreeMap::new();
        let mut spellings: BTreeMap<String, &str> = BTreeMap::new();
        for path in added {
            let folded = path.to_lowercase();
            // Same file under another spelling, or a file over a directory.
            if self.files.get(&folded).is_some_and(|p| p != path)
                || self.directories.contains(&folded)
                || self.ancestors.contains(&folded)
            {
                return false;
            }
            if files.insert(folded, path).is_some() {
                return false; // two spellings among the added files
            }
            for prefix in ancestors(path).chain([*path]) {
                let folded = prefix.to_lowercase();
                if self.spellings.get(&folded).is_some_and(|s| s != prefix)
                    || spellings
                        .insert(folded, prefix)
                        .is_some_and(|s| s != prefix)
                {
                    return false;
                }
            }
        }
        // A file that is an ancestor of another path. Indexed files are not
        // ancestors of indexed paths (the index passed); the rest involve an
        // added path, as the file (`self.ancestors`, above) or as the
        // descendant (here).
        !added.iter().any(|path| {
            ancestors(path).any(|prefix| {
                let folded = prefix.to_lowercase();
                self.files.contains_key(&folded) || files.contains_key(&folded)
            })
        })
    }
}
