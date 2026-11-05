//! Case-insensitive, case-preserving name resolution over the case-sensitive
//! core namespace (for the WinFsp frontend). Existing entries win regardless
//! of case, so the namespace never gets names that differ only in case.
//!
//! Case-folded matches come from a [`CaseIndex`], which answers exactly as a
//! scan of `readdir` would at that moment (it is keyed by the core's view
//! token), so a name created, deleted or renamed locally or by a peer is seen
//! by the next lookup.
use super::super::fs_core::{CaseIndex, FsCore};

/// `parent/name`, or `name` at the root.
pub(super) fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

/// The existing entry named `name` in `parent`, ignoring case (exact wins,
/// an ambiguous match counts as none).
pub(super) fn find(core: &FsCore, index: &CaseIndex, parent: &str, name: &str) -> Option<String> {
    let exact = join(parent, name);
    if core.lookup(&exact).is_ok() {
        return Some(exact);
    }
    folded(index, core, parent, name)
}

/// The one entry of `parent` equal to `name` ignoring case.
fn folded(index: &CaseIndex, core: &FsCore, parent: &str, name: &str) -> Option<String> {
    index
        .unique(core, parent, &name.to_lowercase())
        .map(|entry| join(parent, &entry))
}

/// Resolve every component; the first missing one and the rest keep their spelling.
///
/// Shortcuts that give the same result as resolving component by component:
/// a path that exists as given resolves to itself (each ancestor is then
/// listed in its parent under its exact name, so no component can match a
/// different spelling uniquely), and so does a parent that exists, leaving
/// only the leaf to match.
pub(super) fn resolve(core: &FsCore, index: &CaseIndex, path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let given = parts.join("/");
    if core.lookup(&given).is_ok() {
        return given;
    }
    if let Some((leaf, ancestors)) = parts.split_last() {
        let parent = ancestors.join("/");
        if parent.is_empty() || core.lookup(&parent).is_ok() {
            // The exact leaf was just looked up.
            return folded(index, core, &parent, leaf).unwrap_or(given);
        }
    }
    let mut resolved = String::new();
    let mut parts = parts.into_iter();
    for part in parts.by_ref() {
        match find(core, index, &resolved, part) {
            Some(existing) => resolved = existing,
            None => {
                resolved = join(&resolved, part);
                break;
            }
        }
    }
    for rest in parts {
        resolved = join(&resolved, rest);
    }
    resolved
}

/// Resolved parent and the leaf as given.
pub(super) fn split(core: &FsCore, index: &CaseIndex, path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (resolve(core, index, parent), leaf.into()),
        None => (String::new(), path.into()),
    }
}

/// The entries of `dots` and `listing` (each sorted bytewise) in one
/// bytewise order, strictly after `marker`; a dot entry comes before an
/// equal name. This is what sorting both together (dots first) and skipping
/// up to the marker gives, without copying or sorting the listing per page.
pub(super) fn page<'a, T>(
    dots: &'a [(String, T)],
    listing: &'a [(String, T)],
    marker: Option<&str>,
) -> impl Iterator<Item = &'a (String, T)> + 'a {
    let start = |entries: &'a [(String, T)]| match marker {
        Some(m) => &entries[entries.partition_point(|(name, _)| name.as_str() <= m)..],
        None => entries,
    };
    let (mut dots, mut listing) = (
        start(dots).iter().peekable(),
        start(listing).iter().peekable(),
    );
    std::iter::from_fn(move || match (dots.peek(), listing.peek()) {
        (Some(dot), Some(entry)) if entry.0 < dot.0 => listing.next(),
        (Some(_), _) => dots.next(),
        (None, _) => listing.next(),
    })
}

/// Where a write lands: `offset` (`None` = end of file). Paging I/O
/// (`constrained`) never extends the file, so it is clipped at `size`.
pub(super) fn write_window(
    offset: Option<u64>,
    constrained: bool,
    size: u64,
    len: usize,
) -> (u64, usize) {
    let offset = offset.unwrap_or(size);
    if !constrained {
        return (offset, len);
    }
    if offset >= size {
        return (offset, 0);
    }
    (offset, (len as u64).min(size - offset) as usize)
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
