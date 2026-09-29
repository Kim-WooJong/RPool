//! Case-insensitive, case-preserving name resolution over the case-sensitive
//! core namespace (for the WinFsp frontend). Existing entries win regardless
//! of case, so the namespace never gets names that differ only in case.
use super::super::fs_core::FsCore;

pub(super) fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

/// The existing entry named `name` in `parent`, ignoring case (exact wins,
/// an ambiguous match counts as none).
pub(super) fn find(core: &FsCore, parent: &str, name: &str) -> Option<String> {
    let exact = join(parent, name);
    if core.lookup(&exact).is_ok() {
        return Some(exact);
    }
    let folded = name.to_lowercase();
    let entries = core.readdir(parent).ok()?;
    let mut matches = entries
        .into_iter()
        .filter(|(entry, _)| entry.to_lowercase() == folded);
    let first = matches.next()?;
    matches.next().is_none().then(|| join(parent, &first.0))
}

/// Resolve every component; the first missing one and the rest keep their spelling.
pub(super) fn resolve(core: &FsCore, path: &str) -> String {
    let mut resolved = String::new();
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    for part in parts.by_ref() {
        match find(core, &resolved, part) {
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
pub(super) fn split(core: &FsCore, path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (resolve(core, parent), leaf.into()),
        None => (String::new(), path.into()),
    }
}

/// Directory entries in one bytewise order, strictly after `marker`.
pub(super) fn after_marker<T>(
    mut listing: Vec<(String, T)>,
    marker: Option<&str>,
) -> Vec<(String, T)> {
    listing.sort_by(|a, b| a.0.cmp(&b.0));
    listing.retain(|(name, _)| marker.is_none_or(|m| name.as_str() > m));
    listing
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
mod tests {
    use super::*;
    use crate::mount::fs_core::Access;
    use crate::mount::virtual_drive::fixture;
    use crate::prelude::*;

    fn core_with(files: &[&str]) -> (tempfile::TempDir, FsCore) {
        let root = tempfile::tempdir().unwrap();
        let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
        let write = Access::Write {
            truncate: true,
            append: false,
        };
        for file in files {
            let h = core.open(file, write, true, false).unwrap();
            core.write_at(h, 0, b"x").unwrap();
            core.release(h).unwrap();
        }
        (root, core)
    }

    #[test]
    fn names_resolve_onto_existing_entries_ignoring_case() {
        let (_root, core) = core_with(&["Docs/Report.TXT", "Docs/sub/a", "twin"]);
        assert_eq!(resolve(&core, "docs/report.txt"), "Docs/Report.TXT");
        assert_eq!(resolve(&core, "DOCS/SUB/A"), "Docs/sub/a");
        assert_eq!(resolve(&core, "docs/New File"), "Docs/New File");
        assert_eq!(resolve(&core, "missing/Deeper/x"), "missing/Deeper/x");
        assert_eq!(resolve(&core, "TWIN"), "twin");
        // The drive itself refuses names that differ only in case.
        let write = Access::Write {
            truncate: true,
            append: false,
        };
        let twin = core.open("Twin", write, true, false).unwrap();
        core.write_at(twin, 0, b"y").unwrap();
        assert!(core.release(twin).is_err());
        assert_eq!(find(&core, "", "TWIN").as_deref(), Some("twin"));
        assert_eq!(
            split(&core, "docs/NEW.txt"),
            ("Docs".into(), "NEW.txt".into())
        );
        assert_eq!(split(&core, "top"), (String::new(), "top".into()));
        assert_eq!(
            find(&core, "Docs", "report.txt").as_deref(),
            Some("Docs/Report.TXT")
        );
    }

    #[test]
    fn listings_resume_strictly_after_the_marker_in_one_order() {
        let names = ["b", ".", "-dash", "..", "a", "B"];
        let listing: Vec<(String, ())> = names.iter().map(|n| (n.to_string(), ())).collect();
        let all: Vec<String> = after_marker(listing.clone(), None)
            .into_iter()
            .map(|e| e.0)
            .collect();
        assert_eq!(all, ["-dash", ".", "..", "B", "a", "b"]);
        let rest: Vec<String> = after_marker(listing.clone(), Some(".."))
            .into_iter()
            .map(|e| e.0)
            .collect();
        assert_eq!(rest, ["B", "a", "b"]);
        // A marker that vanished still resumes at the next name.
        let rest: Vec<String> = after_marker(listing, Some("AA"))
            .into_iter()
            .map(|e| e.0)
            .collect();
        assert_eq!(rest, ["B", "a", "b"]);
    }

    #[test]
    fn paging_writes_never_extend_the_file() {
        assert_eq!(write_window(Some(10), false, 4, 8), (10, 8));
        assert_eq!(write_window(None, false, 4, 8), (4, 8));
        assert_eq!(write_window(Some(2), true, 4, 8), (2, 2));
        assert_eq!(write_window(Some(4), true, 4, 8), (4, 0));
        assert_eq!(write_window(Some(9), true, 4, 8), (9, 0));
        assert_eq!(write_window(Some(0), true, 4096, 512), (0, 512));
    }
}
