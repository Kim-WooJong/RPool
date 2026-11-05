//! Name and listing caches for frontends, keyed by a view token.
//!
//! What `readdir` returns is a function of two things only: the drive's
//! visible namespace, fixed by the namespace revision (`StateLock::revision`,
//! advanced by every local change, upload commit and peer pull), and this
//! core's unsealed generations (path, id, size, intent). [`ViewToken`] holds
//! the revision, each linked unsealed generation's summary stamp (which
//! changes on every size or intent change and is never reused in this core)
//! and the core's generation presence counter (which catches a generation
//! that appears and disappears between two readings), so equal tokens read
//! before and after building a result mean nothing it depends on changed
//! meanwhile, not even a change that was then undone. Other cores (mounts)
//! have their own counters and never invalidate this one's results. A cached
//! result is used only while the current token equals the one it was built
//! under, and it is stored only if the token read before and after
//! building it is the same, so a create, delete, rename, seal, write, truncate
//! or peer pull is seen by the next lookup exactly as without a cache. Session
//! pins (the local-only fixture) change the view outside the revision, so
//! nothing is cached while they apply.
use super::core::{lock, Attr, FsCore};
use super::error::{FsError, FsResult};
use super::identity::FileId;
use super::slot::Slot;
use crate::prelude::*;
use std::collections::HashMap;

/// Directories a [`CaseIndex`] keeps at once.
const INDEXED_DIRECTORIES: usize = 1024;
/// Names a [`CaseIndex`] keeps at once, over all its directories.
const INDEXED_NAMES: usize = 1 << 18;
/// Directories a [`ListingCache`] keeps at once.
const LISTED_DIRECTORIES: usize = 64;
/// Entries a [`ListingCache`] keeps at once, over all its directories.
const LISTED_ENTRIES: usize = 1 << 17;

/// Everything `readdir` depends on (see the module comment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewToken {
    /// This core's generation presence counter, read first.
    presence: u64,
    /// Namespace revision of the drive.
    revision: u64,
    /// Linked unsealed generations: `(id, path, intent id, summary stamp)`, by id.
    unsealed: Vec<(FileId, String, String, u64)>,
}
impl ViewToken {
    /// Same names: equal except, possibly, for summary stamps (sizes).
    fn same_names(&self, other: &Self) -> bool {
        self.presence == other.presence
            && self.revision == other.revision
            && self.unsealed.len() == other.unsealed.len()
            && self
                .unsealed
                .iter()
                .zip(&other.unsealed)
                .all(|(a, b)| a.0 == b.0 && a.1 == b.1 && a.2 == b.2)
    }
}

impl FsCore {
    /// The current [`ViewToken`]; `None` when results must not be cached
    /// (session pins apply, or a lock is poisoned).
    pub(crate) fn view_token(&self) -> FsResult<Option<ViewToken>> {
        let pinned = match self.drive.pins.lock() {
            Ok(pins) => !pins.is_empty(),
            Err(_) => return Ok(None),
        };
        if self.drive.pool_sync_roots.is_empty() && pinned {
            return Ok(None);
        }
        // Presence first: a generation that appears after this reading, even
        // in a slot not yet in the map, advances it before it can be read.
        let presence = self.counters.presence();
        let Some(revision) = self.drive.state.revision() else {
            return Ok(None);
        };
        // Leaf locks one at a time: slot map, then each summary, then ids.
        let slots: Vec<(FileId, Slot)> = lock(&self.slots)?
            .iter()
            .map(|(file, slot)| (*file, slot.clone()))
            .collect();
        let mut unsealed = Vec::new();
        for (file, slot) in slots {
            let (Some((_, intent)), stamp) = slot.stamped()? else {
                continue;
            };
            if let Some(path) = lock(&self.ids)?.path(file) {
                unsealed.push((file, path, intent, stamp));
            }
        }
        Ok(Some(ViewToken {
            presence,
            revision,
            unsealed,
        }))
    }
}

/// How often a cache answered from memory or had to list a directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheStats {
    /// Answered from a cached directory.
    pub(crate) hits: u64,
    /// Listed the directory (a `readdir`), whether or not it was then kept.
    pub(crate) listings: u64,
}

/// Results of one token, by directory, with their total weight.
struct Entries<V> {
    /// Token the values were built under.
    token: Option<ViewToken>,
    /// Each directory's value and weight.
    directories: HashMap<String, (Arc<V>, usize)>,
    /// Sum of the weights.
    weight: usize,
    /// Counts since the cache was created.
    stats: CacheStats,
}
impl<V> Default for Entries<V> {
    fn default() -> Self {
        Self {
            token: None,
            directories: HashMap::new(),
            weight: 0,
            stats: CacheStats::default(),
        }
    }
}

/// A token-keyed cache of one value per directory.
struct TokenCache<V> {
    /// The cached values and counts.
    entries: Mutex<Entries<V>>,
    /// Whether a value built under one token is still valid under another.
    valid: fn(&ViewToken, &ViewToken) -> bool,
    /// Directories kept at once.
    max_directories: usize,
    /// Total weight kept at once.
    max_weight: usize,
}
impl<V> TokenCache<V> {
    /// An empty cache whose values stay `valid` across tokens as given.
    fn new(
        valid: fn(&ViewToken, &ViewToken) -> bool,
        max_directories: usize,
        max_weight: usize,
    ) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            valid,
            max_directories,
            max_weight,
        }
    }
    /// A cache poisoned by a panic starts over empty.
    fn entries(&self) -> std::sync::MutexGuard<'_, Entries<V>> {
        self.entries.lock().unwrap_or_else(|poisoned| {
            let mut entries = poisoned.into_inner();
            let stats = entries.stats;
            *entries = Entries::default();
            entries.stats = stats;
            entries
        })
    }
    /// The value of `directory` under the current token; `build` makes it
    /// (with its weight) when it is missing or stale.
    fn get(
        &self,
        core: &FsCore,
        directory: &str,
        build: impl FnOnce() -> FsResult<(V, usize)>,
    ) -> FsResult<Arc<V>> {
        // A token that cannot be read only means no caching.
        let before = core.view_token().ok().flatten();
        if let Some(token) = &before {
            let mut entries = self.entries();
            let fresh = entries
                .token
                .as_ref()
                .is_some_and(|t| (self.valid)(t, token));
            let cached = entries.directories.get(directory).map(|(v, _)| v.clone());
            if let (true, Some(value)) = (fresh, cached) {
                entries.stats.hits += 1;
                return Ok(value);
            }
        }
        self.entries().stats.listings += 1;
        let (value, weight) = build()?;
        let value = Arc::new(value);
        let Some(before) = before else {
            return Ok(value);
        };
        // Keep it only if nothing it depends on changed while it was built.
        let after = core.view_token().ok().flatten();
        if after.as_ref() != Some(&before) || weight > self.max_weight {
            return Ok(value);
        }
        let mut entries = self.entries();
        let current = entries
            .token
            .as_ref()
            .is_some_and(|t| (self.valid)(t, &before));
        if !current {
            if entries
                .token
                .as_ref()
                .is_some_and(|t| t.revision > before.revision)
            {
                // Built under an older namespace than the cache already holds.
                return Ok(value);
            }
            entries.directories.clear();
            entries.weight = 0;
            entries.token = Some(before);
        }
        if let Some((_, old)) = entries.directories.remove(directory) {
            entries.weight -= old;
        }
        if entries.directories.len() >= self.max_directories
            || entries.weight + weight > self.max_weight
        {
            entries.directories.clear();
            entries.weight = 0;
        }
        entries.weight += weight;
        entries
            .directories
            .insert(directory.to_owned(), (value.clone(), weight));
        Ok(value)
    }
    /// Counts since the cache was created.
    fn stats(&self) -> CacheStats {
        self.entries().stats
    }
}

/// Lowercased name → the one entry with that lowercase, `None` if several.
type Folded = HashMap<String, Option<String>>;

/// Case-insensitive name index of directories, for frontends that resolve
/// names ignoring case. Answers exactly as scanning `readdir` would.
pub(crate) struct CaseIndex(TokenCache<Folded>);
impl Default for CaseIndex {
    fn default() -> Self {
        // Sizes do not change names.
        Self(TokenCache::new(
            ViewToken::same_names,
            INDEXED_DIRECTORIES,
            INDEXED_NAMES,
        ))
    }
}
impl CaseIndex {
    /// The one entry of `directory` whose lowercase is `folded`, in
    /// `readdir`'s spelling; `None` when there is none, there are several,
    /// or `directory` is not a directory.
    pub(crate) fn unique(&self, core: &FsCore, directory: &str, folded: &str) -> Option<String> {
        let index = self
            .0
            .get(core, directory, || {
                let entries = match core.readdir(directory) {
                    Ok(entries) => entries,
                    Err(FsError::NotFound | FsError::NotDir) => vec![],
                    Err(error) => return Err(error),
                };
                let mut folded = Folded::with_capacity(entries.len());
                for (name, _) in entries {
                    folded
                        .entry(name.to_lowercase())
                        .and_modify(|unique| *unique = None)
                        .or_insert(Some(name));
                }
                let weight = folded.len().max(1);
                Ok((folded, weight))
            })
            .ok()?;
        index.get(folded).cloned().flatten()
    }
    /// Hit and listing counts so far.
    pub(crate) fn stats(&self) -> CacheStats {
        self.0.stats()
    }
}

/// Directory listings for paged enumeration: one listing serves every page
/// while nothing in the directory (including unsealed sizes) changed.
/// Entries carry no file ids (`Attr::id` is `None`): a visible file's id is
/// allocated by its first open, which the token does not track.
pub(crate) struct ListingCache(TokenCache<Vec<(String, Attr)>>);
impl Default for ListingCache {
    fn default() -> Self {
        Self(TokenCache::new(
            ViewToken::eq,
            LISTED_DIRECTORIES,
            LISTED_ENTRIES,
        ))
    }
}
impl ListingCache {
    /// `readdir(directory)` without ids, sorted bytewise by name.
    pub(crate) fn listing(
        &self,
        core: &FsCore,
        directory: &str,
    ) -> FsResult<Arc<Vec<(String, Attr)>>> {
        self.0.get(core, directory, || {
            let mut entries = core.readdir(directory)?;
            for (_, attr) in &mut entries {
                attr.id = None;
            }
            let weight = entries.len().max(1);
            Ok((entries, weight))
        })
    }
    /// Hit and listing counts so far.
    pub(crate) fn stats(&self) -> CacheStats {
        self.0.stats()
    }
}

#[cfg(test)]
#[path = "view_cache_tests.rs"]
mod tests;
