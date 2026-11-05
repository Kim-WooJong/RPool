use super::*;
use crate::mount::fs_core::{Access, CacheStats, ListingCache};
use crate::mount::virtual_drive::fixture;
use crate::prelude::*;

const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

fn put(core: &FsCore, file: &str) {
    let h = core.open(file, TRUNC, true, false).unwrap();
    core.write_at(h, 0, b"x").unwrap();
    core.release(h).unwrap();
}
/// A local-only core (session pins: nothing cached) or a pool-sync one.
fn core_with(peer: bool, files: &[&str]) -> (tempfile::TempDir, FsCore) {
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    if peer {
        drive.pool_sync_roots = vec!["crypt:pool".into()];
        drive.state.lock().unwrap().version = 6;
    }
    let core = FsCore::new(Arc::new(drive)).unwrap();
    for file in files {
        put(&core, file);
    }
    (root, core)
}

/// The resolution before the cache and shortcuts: every component looked up,
/// then matched by scanning a fresh `readdir`.
mod reference {
    use super::super::join;
    use crate::mount::fs_core::FsCore;
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
    /// Sort dots then entries together, keep what follows the marker.
    pub(super) fn after_marker<T: Clone>(
        dots: &[(String, T)],
        listing: &[(String, T)],
        marker: Option<&str>,
    ) -> Vec<(String, T)> {
        let mut all: Vec<_> = dots.iter().chain(listing).cloned().collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all.retain(|(name, _)| marker.is_none_or(|m| name.as_str() > m));
        all
    }
}

#[test]
fn names_resolve_onto_existing_entries_ignoring_case() {
    for peer in [false, true] {
        let (_root, core) = core_with(peer, &["Docs/Report.TXT", "Docs/sub/a", "twin"]);
        let index = CaseIndex::default();
        assert_eq!(resolve(&core, &index, "docs/report.txt"), "Docs/Report.TXT");
        assert_eq!(resolve(&core, &index, "DOCS/SUB/A"), "Docs/sub/a");
        assert_eq!(resolve(&core, &index, "docs/New File"), "Docs/New File");
        assert_eq!(
            resolve(&core, &index, "missing/Deeper/x"),
            "missing/Deeper/x"
        );
        assert_eq!(resolve(&core, &index, "TWIN"), "twin");
        assert_eq!(resolve(&core, &index, "/Docs//sub/"), "Docs/sub");
        assert_eq!(resolve(&core, &index, ""), "");
        // The drive itself refuses names that differ only in case.
        let twin = core.open("Twin", TRUNC, true, false).unwrap();
        core.write_at(twin, 0, b"y").unwrap();
        assert!(core.release(twin).is_err());
        assert_eq!(find(&core, &index, "", "TWIN").as_deref(), Some("twin"));
        assert_eq!(
            split(&core, &index, "docs/NEW.txt"),
            ("Docs".into(), "NEW.txt".into())
        );
        assert_eq!(split(&core, &index, "top"), (String::new(), "top".into()));
        assert_eq!(
            find(&core, &index, "Docs", "report.txt").as_deref(),
            Some("Docs/Report.TXT")
        );
    }
}

#[test]
fn resolution_matches_the_component_walk() {
    let (_root, core) = core_with(
        true,
        &[
            "Docs/Report.TXT",
            "Docs/sub/a",
            "Docs/sub/B",
            "twin",
            "x/y/z",
        ],
    );
    core.mkdir("Empty/Inner").unwrap();
    let unsealed = core.open("Docs/sub/Open.txt", TRUNC, true, false).unwrap();
    let index = CaseIndex::default();
    let paths = [
        "",
        "docs",
        "DOCS/sub",
        "Docs/sub/a",
        "docs/sub/A",
        "docs/sub/b",
        "docs/sub/open.TXT",
        "docs/sub/open.txt/deeper",
        "Docs/Report.TXT/x",
        "twin/x",
        "TWIN",
        "empty",
        "empty/inner",
        "EMPTY/INNER/new",
        "X/Y/Z",
        "x/Y/missing",
        "nowhere/a/b",
        "Docs/nowhere/b",
    ];
    for path in paths {
        assert_eq!(
            resolve(&core, &index, path),
            reference::resolve(&core, path),
            "{path}"
        );
    }
    core.release(unsealed).unwrap();
}

#[test]
fn created_and_renamed_names_are_seen_by_the_next_lookup() {
    let (_root, core) = core_with(true, &["Src/main.rs"]);
    let index = CaseIndex::default();
    // A compiler probes for an output, then creates it in another case.
    assert_eq!(find(&core, &index, "Src", "main.o"), None);
    assert_eq!(
        split(&core, &index, "src/main.o"),
        ("Src".into(), "main.o".into())
    );
    let h = core.open("Src/Main.O", TRUNC, true, false).unwrap();
    assert_eq!(
        find(&core, &index, "Src", "main.o").as_deref(),
        Some("Src/Main.O")
    );
    assert_eq!(resolve(&core, &index, "SRC/MAIN.o"), "Src/Main.O");
    core.release(h).unwrap();
    // An editor's save: write a temp, rename it over, delete the backup.
    put(&core, "Src/main.rs.tmp");
    core.rename("Src/main.rs", "Src/main.rs.bak").unwrap();
    core.rename("Src/main.rs.tmp", "Src/main.rs").unwrap();
    assert_eq!(resolve(&core, &index, "src/MAIN.RS.TMP"), "Src/MAIN.RS.TMP");
    assert_eq!(resolve(&core, &index, "src/main.RS"), "Src/main.rs");
    core.delete("Src/main.rs.bak").unwrap();
    assert_eq!(find(&core, &index, "Src", "MAIN.RS.BAK"), None);
    // A directory rename moves everything below it.
    core.rename("Src", "Lib").unwrap();
    assert_eq!(resolve(&core, &index, "src/main.rs"), "src/main.rs");
    assert_eq!(resolve(&core, &index, "LIB/MAIN.RS"), "Lib/main.rs");
}

#[test]
fn listings_resume_strictly_after_the_marker_in_one_order() {
    let named = |names: &[&str]| -> Vec<(String, usize)> {
        let mut v: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.to_string(), i))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let dots = named(&[".", ".."]);
    let listing = named(&["b", "-dash", "a", "B", ".hidden", "..x", "~", "é", "A b"]);
    let names = |v: Vec<&(String, usize)>| v.into_iter().map(|e| e.0.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(page(&dots, &listing, None).collect()),
        ["-dash", ".", "..", "..x", ".hidden", "A b", "B", "a", "b", "~", "é"]
    );
    let mut markers: Vec<String> = dots.iter().chain(&listing).map(|e| e.0.clone()).collect();
    // Markers that vanished still resume at the next name.
    markers.extend(["", " ", "AA", "zzz", "\u{10FFFF}", "..y", "-"].map(String::from));
    for dots in [&dots[..], &[][..]] {
        for marker in markers.iter().map(Some).chain([None]) {
            let marker = marker.map(String::as_str);
            let paged: Vec<(String, usize)> = page(dots, &listing, marker).cloned().collect();
            assert_eq!(
                paged,
                reference::after_marker(dots, &listing, marker),
                "{marker:?}"
            );
        }
    }
    // Equal names: the dot entry first, as a stable sort of dots then entries.
    let clash = named(&["."]);
    let tagged: Vec<_> = page(&dots, &clash, None)
        .map(|e| (e.0.as_str(), e.1))
        .collect();
    assert_eq!(tagged, [(".", 0), (".", 0), ("..", 1)]);
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

/// Local synthetic comparison of the old and new lookup and listing paths.
/// Asserts work counts (directory listings), prints timings for information:
/// `cargo test --lib frontend::names::tests::compare -- --ignored --nocapture`.
#[test]
#[ignore = "synthetic performance comparison; run explicitly"]
fn compare_lookup_and_listing_paths() {
    use std::time::Instant;
    const FILES: usize = 1000;
    const PROBES: usize = 500;
    const PAGE: usize = 64;
    let root = tempfile::tempdir().unwrap();
    let mut drive = fixture(root.path());
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    drive.state.lock().unwrap().version = 6;
    let drive = Arc::new(drive);
    let core = FsCore::new(drive.clone()).unwrap();
    let start = Instant::now();
    for i in 0..FILES {
        put(&core, &format!("Project/node_modules/pkg{i:05}/index.js"));
    }
    put(&core, "Project/src/Main.ts");
    // Uploaded, as a synced tree is: committed files, no pending overlay.
    let pending = drive.state.lock().unwrap().pending.clone();
    for intent in pending {
        let bytes = std::fs::read(drive.spool_path(&intent)).unwrap();
        let content = crate::mount::virtual_tests::content(&bytes);
        drive.commit_uploaded(&intent, Some(content)).unwrap();
    }
    assert!(drive.state.lock().unwrap().pending.is_empty());
    println!("setup of {FILES} committed files: {:?}", start.elapsed());
    let probes: Vec<String> = (0..PROBES)
        .map(|i| format!("project/node_modules/Missing{i}.json"))
        .collect();

    // Negative / case-folded lookups (editor and compiler probes).
    let start = Instant::now();
    let old: Vec<String> = probes
        .iter()
        .map(|p| reference::resolve(&core, p))
        .collect();
    let old_time = start.elapsed();
    let index = CaseIndex::default();
    let start = Instant::now();
    let new: Vec<String> = probes.iter().map(|p| resolve(&core, &index, p)).collect();
    let new_time = start.elapsed();
    assert_eq!(old, new);
    // Old: every probe lists Project (to fold "project") and node_modules.
    // New: each directory is listed once, then answered from the index.
    assert_eq!(
        index.stats(),
        CacheStats {
            hits: 2 * PROBES as u64 - 2,
            listings: 2
        }
    );
    println!(
        "{PROBES} folded/negative probes in a {FILES}-entry directory: \
         old {old_time:?} ({} listings), new {new_time:?} (1 listing per directory)",
        2 * PROBES
    );

    // Exact existing paths: one lookup instead of one per component.
    let start = Instant::now();
    for _ in 0..PROBES {
        assert_eq!(
            reference::resolve(&core, "Project/src/Main.ts"),
            "Project/src/Main.ts"
        );
    }
    let old_time = start.elapsed();
    let start = Instant::now();
    for _ in 0..PROBES {
        assert_eq!(
            resolve(&core, &index, "Project/src/Main.ts"),
            "Project/src/Main.ts"
        );
    }
    println!(
        "{PROBES} exact 3-component resolves: old {old_time:?} (3 lookups each), new {:?} (1)",
        start.elapsed()
    );

    // Paged enumeration of the big directory.
    let directory = "Project/node_modules";
    let dots: Vec<(String, crate::mount::fs_core::Attr)> = vec![];
    let start = Instant::now();
    let (mut old_pages, mut old_names, mut marker) = (0, Vec::new(), None::<String>);
    loop {
        let listing = core.readdir(directory).unwrap();
        let rest = reference::after_marker(&dots, &listing, marker.as_deref());
        old_pages += 1;
        if rest.is_empty() {
            break;
        }
        let chunk: Vec<String> = rest.into_iter().take(PAGE).map(|e| e.0).collect();
        marker = chunk.last().cloned();
        old_names.extend(chunk);
    }
    let old_time = start.elapsed();
    let listings = ListingCache::default();
    let start = Instant::now();
    let (mut new_pages, mut new_names, mut marker) = (0, Vec::new(), None::<String>);
    loop {
        let listing = listings.listing(&core, directory).unwrap();
        let chunk: Vec<String> = page(&dots, &listing, marker.as_deref())
            .take(PAGE)
            .map(|e| e.0.clone())
            .collect();
        new_pages += 1;
        if chunk.is_empty() {
            break;
        }
        marker = chunk.last().cloned();
        new_names.extend(chunk);
    }
    let new_time = start.elapsed();
    assert_eq!(old_names, new_names);
    assert_eq!(new_names.len(), FILES);
    assert_eq!(old_pages, new_pages);
    assert_eq!(
        listings.stats(),
        CacheStats {
            hits: new_pages - 1,
            listings: 1
        }
    );
    println!(
        "{FILES} entries in {new_pages} pages of {PAGE}: old {old_time:?} ({old_pages} listings \
         and sorts), new {new_time:?} (1 listing)"
    );
}
