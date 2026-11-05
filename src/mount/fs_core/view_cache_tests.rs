//! The view token changes with everything `readdir` depends on, and the
//! caches answer exactly as a fresh `readdir` scan after each kind of change:
//! unsealed create, write, truncate, seal, rename, delete, mkdir, rmdir and
//! peer pulls (new file, case twin, deletion).
use super::*;
use crate::mount::fs_core::{Access, HandleId};
use crate::mount::shared_model::Event;
use crate::mount::virtual_drive::{fixture, VirtualDrive};
use crate::mount::virtual_tests::content;

const W: Access = Access::Write {
    truncate: false,
    append: false,
};
const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

/// A pool-sync core (no session pins, so results are cacheable).
fn peer_core(root: &Path) -> FsCore {
    let mut drive = fixture(root);
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    drive.state.lock().unwrap().version = 6;
    FsCore::new(Arc::new(drive)).unwrap()
}
fn put(core: &FsCore, path: &str, bytes: &[u8]) {
    let handle = core.open(path, TRUNC, true, false).unwrap();
    core.write_at(handle, 0, bytes).unwrap();
    core.release(handle).unwrap();
}
/// A peer PC's new file (empty) or deletion at `path`, as a sync pull adds it.
fn peer_edit(drive: &VirtualDrive, path: &str, parents: Vec<String>, deleted: bool) -> String {
    let event = Event {
        version: 1,
        device: "peer-device".into(),
        worker: "peer".into(),
        path: path.into(),
        parents,
        content: (!deleted).then(|| content(b"")),
    };
    let id = event.id().unwrap();
    drive.state.lock().unwrap().events.insert(id.clone(), event);
    id
}
/// What scanning a fresh `readdir` answers (the pre-cache algorithm).
fn scan(core: &FsCore, directory: &str, folded: &str) -> Option<String> {
    let entries = core.readdir(directory).ok()?;
    let mut matches = entries
        .into_iter()
        .filter(|(entry, _)| entry.to_lowercase() == folded);
    let first = matches.next()?;
    matches.next().is_none().then_some(first.0)
}
/// The index agrees with a fresh scan for every probe.
#[track_caller]
fn agrees(core: &FsCore, index: &CaseIndex, listings: &ListingCache) {
    for (directory, name) in [
        ("", "docs"),
        ("", "new.txt"),
        ("", "moved.txt"),
        ("", "peer.txt"),
        ("", "twin"),
        ("docs", "report.txt"),
        ("docs", "draft.txt"),
        ("docs", "sub"),
        ("missing", "x"),
        ("docs/report.txt", "x"),
    ] {
        assert_eq!(
            index.unique(core, directory, name),
            scan(core, directory, name),
            "{directory}/{name}"
        );
    }
    for directory in ["", "docs", "docs/sub"] {
        let fresh = core.readdir(directory).ok().map(|mut entries| {
            for (_, attr) in &mut entries {
                attr.id = None;
            }
            entries
        });
        let cached = listings.listing(core, directory).ok();
        assert_eq!(
            cached.as_deref(),
            fresh.as_ref(),
            "listing of {directory:?}"
        );
    }
}
fn token(core: &FsCore) -> ViewToken {
    core.view_token().unwrap().unwrap()
}

#[test]
fn every_change_readdir_sees_changes_the_token() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    let t0 = token(&core);
    assert_eq!(token(&core), t0, "reading changes nothing");
    let h = core.open("New.txt", W, true, false).unwrap();
    let created = token(&core);
    assert!(!created.same_names(&t0), "unsealed create");
    core.write_at(h, 0, b"abc").unwrap();
    let written = token(&core);
    assert_ne!(written, created, "unsealed write changes a summary stamp");
    assert!(written.same_names(&created), "but not names");
    core.truncate(h, 1).unwrap();
    let truncated = token(&core);
    assert_ne!(truncated, written);
    assert!(truncated.same_names(&written));
    core.release(h).unwrap();
    let sealed = token(&core);
    assert!(!sealed.same_names(&truncated), "seal");
    core.rename("New.txt", "moved.txt").unwrap();
    let renamed = token(&core);
    assert!(!renamed.same_names(&sealed), "rename");
    core.delete("moved.txt").unwrap();
    let deleted = token(&core);
    assert!(!deleted.same_names(&renamed), "delete");
    core.mkdir("empty").unwrap();
    let made = token(&core);
    assert!(!made.same_names(&deleted), "mkdir");
    peer_edit(&core.drive, "peer.txt", vec![], false);
    assert!(!token(&core).same_names(&made), "peer pull");
}

#[test]
fn caches_answer_as_a_fresh_scan_through_every_change() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    let (index, listings) = (CaseIndex::default(), ListingCache::default());
    put(&core, "Docs/Report.TXT", b"r");
    put(&core, "Docs/sub/a", b"a");
    agrees(&core, &index, &listings);
    // Warm a negative answer, then create the name unsealed (no namespace change).
    assert_eq!(index.unique(&core, "", "new.txt"), None);
    let h: HandleId = core.open("NEW.txt", W, true, false).unwrap();
    assert_eq!(
        index.unique(&core, "", "new.txt").as_deref(),
        Some("NEW.txt")
    );
    agrees(&core, &index, &listings);
    // Unsealed writes and truncation change listed sizes only.
    core.write_at(h, 0, b"12345").unwrap();
    let size = |l: &ListingCache| {
        l.listing(&core, "")
            .unwrap()
            .iter()
            .find(|(n, _)| n == "NEW.txt")
            .map(|(_, a)| a.size)
    };
    assert_eq!(size(&listings), Some(5));
    core.truncate(h, 2).unwrap();
    assert_eq!(size(&listings), Some(2));
    agrees(&core, &index, &listings);
    core.release(h).unwrap();
    agrees(&core, &index, &listings);
    // Rename, delete, mkdir, rmdir.
    core.rename("NEW.txt", "Moved.txt").unwrap();
    assert_eq!(index.unique(&core, "", "new.txt"), None);
    assert_eq!(
        index.unique(&core, "", "moved.txt").as_deref(),
        Some("Moved.txt")
    );
    agrees(&core, &index, &listings);
    core.delete("Moved.txt").unwrap();
    assert_eq!(index.unique(&core, "", "moved.txt"), None);
    agrees(&core, &index, &listings);
    core.mkdir("Docs/Draft.txt").unwrap();
    assert_eq!(
        index.unique(&core, "Docs", "draft.txt").as_deref(),
        Some("Draft.txt")
    );
    core.rmdir("Docs/Draft.txt").unwrap();
    assert_eq!(index.unique(&core, "Docs", "draft.txt"), None);
    agrees(&core, &index, &listings);
    // Peer pulls: a new file, a case twin (ambiguous), then its deletion.
    assert_eq!(index.unique(&core, "", "peer.txt"), None);
    let peer = peer_edit(&core.drive, "Peer.txt", vec![], false);
    assert_eq!(
        index.unique(&core, "", "peer.txt").as_deref(),
        Some("Peer.txt")
    );
    agrees(&core, &index, &listings);
    let twin = peer_edit(&core.drive, "Docs/report.txt", vec![], false);
    assert_eq!(index.unique(&core, "Docs", "report.txt"), None, "ambiguous");
    agrees(&core, &index, &listings);
    peer_edit(&core.drive, "Docs/report.txt", vec![twin], true);
    assert_eq!(
        index.unique(&core, "Docs", "report.txt").as_deref(),
        Some("Report.TXT")
    );
    peer_edit(&core.drive, "Peer.txt", vec![peer], true);
    assert_eq!(index.unique(&core, "", "peer.txt"), None);
    agrees(&core, &index, &listings);
}

#[test]
fn an_unchanged_view_is_answered_from_memory() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    for i in 0..20 {
        put(&core, &format!("dir/f{i}"), b"x");
    }
    let index = CaseIndex::default();
    for i in 0..100 {
        assert_eq!(index.unique(&core, "dir", &format!("missing{i}")), None);
    }
    assert_eq!(
        index.unique(&core, "dir", "F3").as_deref(),
        None,
        "folded input only"
    );
    assert_eq!(index.unique(&core, "dir", "f3").as_deref(), Some("f3"));
    assert_eq!(
        index.stats(),
        CacheStats {
            hits: 101,
            listings: 1
        }
    );
    // A write elsewhere changes a size only: names stay cached.
    let h = core.open("other", W, true, false).unwrap();
    index.unique(&core, "dir", "x");
    assert_eq!(index.stats().listings, 2, "a create relists");
    core.write_at(h, 0, b"more").unwrap();
    index.unique(&core, "dir", "x");
    assert_eq!(index.stats().listings, 2, "a write does not");
    core.release(h).unwrap();
    let listings = ListingCache::default();
    for _ in 0..10 {
        assert_eq!(listings.listing(&core, "dir").unwrap().len(), 20);
    }
    assert_eq!(
        listings.stats(),
        CacheStats {
            hits: 9,
            listings: 1
        }
    );
    assert!(
        listings.listing(&core, "dir/f1").is_err(),
        "errors pass through"
    );
    assert!(listings.listing(&core, "nowhere").is_err());
}

/// A size that changes and changes back while a listing is built (truncate,
/// then a write): the listing saw the middle size, so it must not be kept,
/// although sizes, intent, revision and presence all read as before.
/// (Each test has its own core, so parallel tests never touch its counters.)
#[test]
fn a_size_changed_and_back_during_a_listing_is_not_kept() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    let h = core.open("f", W, true, false).unwrap();
    core.write_at(h, 0, b"AAAA").unwrap();
    let listings = ListingCache::default();
    let start = token(&core);
    let built = listings
        .0
        .get(&core, "", || {
            core.truncate(h, 2).unwrap();
            let entries = core.readdir("").unwrap();
            core.write_at(h, 2, b"AA").unwrap();
            Ok((entries, 1))
        })
        .unwrap();
    let end = token(&core);
    assert_eq!(built[0].1.size, 2, "built from the middle state");
    assert_eq!(
        listings.listing(&core, "").unwrap()[0].1.size,
        4,
        "the middle state was not kept"
    );
    assert!(end.same_names(&start), "same generation, path and intent");
    assert_ne!(end, start, "but a newer summary stamp");
    assert_eq!(core.lookup("f").unwrap().size, 4);
    core.release(h).unwrap();
}

/// A generation that appears and disappears while an index is built (a
/// delete-on-close temp file whose path already has a recorded base, so
/// starting it saves nothing): the index saw the name, so it is not kept.
#[test]
fn a_generation_that_came_and_went_during_a_build_is_not_kept() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "keep", b"k");
    // The second start at the path reuses the base the first one saved.
    let first = core.open("tmp", W, true, false).unwrap();
    core.delete("tmp").unwrap();
    core.release(first).unwrap();
    let index = CaseIndex::default();
    let revision = core.drive.state.revision();
    let seen = index
        .0
        .get(&core, "", || {
            let temp = core.open("tmp", W, true, false).unwrap();
            let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
            core.delete("tmp").unwrap();
            core.release(temp).unwrap();
            let folded: Folded = names
                .iter()
                .map(|n| (n.to_lowercase(), Some(n.clone())))
                .collect();
            Ok((folded, 2))
        })
        .unwrap();
    assert!(
        seen.contains_key("tmp"),
        "built while the temp file existed"
    );
    assert_eq!(
        core.drive.state.revision(),
        revision,
        "the namespace revision alone does not show it"
    );
    // `unique` takes the lowercased name.
    assert_eq!(index.unique(&core, "", "tmp"), None);
    assert_eq!(index.unique(&core, "", "keep").as_deref(), Some("keep"));
}

#[test]
fn another_cores_writes_never_invalidate_this_cores_caches() {
    let (a_root, b_root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (peer_core(a_root.path()), peer_core(b_root.path()));
    put(&a, "dir/f", b"x");
    let (index, listings) = (CaseIndex::default(), ListingCache::default());
    index.unique(&a, "dir", "f");
    listings.listing(&a, "dir").unwrap();
    let before = token(&a);
    // Generations start, change size, are deleted and sealed in the other core.
    let h = b.open("tmp", W, true, false).unwrap();
    b.write_at(h, 0, b"abc").unwrap();
    b.delete("tmp").unwrap();
    b.release(h).unwrap();
    put(&b, "dir/g", b"y");
    assert_eq!(token(&a), before);
    index.unique(&a, "dir", "f");
    listings.listing(&a, "dir").unwrap();
    assert_eq!(
        index.stats(),
        CacheStats {
            hits: 1,
            listings: 1
        }
    );
    assert_eq!(
        listings.stats(),
        CacheStats {
            hits: 1,
            listings: 1
        }
    );
}

#[test]
fn nothing_is_cached_while_session_pins_apply() {
    let root = tempfile::tempdir().unwrap();
    // The local-only fixture pins what it serves.
    let core = FsCore::new(Arc::new(fixture(root.path()))).unwrap();
    put(&core, "a", b"a");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    core.release(reader).unwrap();
    assert!(core.view_token().unwrap().is_none());
    let index = CaseIndex::default();
    assert_eq!(index.unique(&core, "", "A").as_deref(), None);
    assert_eq!(index.unique(&core, "", "a").as_deref(), Some("a"));
    assert_eq!(index.stats().hits, 0);
    assert_eq!(index.stats().listings, 2);
}

#[test]
fn listed_ids_never_go_stale() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"a");
    let listings = ListingCache::default();
    assert_eq!(listings.listing(&core, "").unwrap()[0].1.id, None);
    // Opening a visible file allocates its id without changing the view.
    let reader = core.open("a", Access::Read, false, false).unwrap();
    assert!(core.readdir("").unwrap()[0].1.id.is_some());
    assert_eq!(listings.listing(&core, "").unwrap()[0].1.id, None);
    core.release(reader).unwrap();
}

#[test]
fn caches_stay_bounded() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    for i in 0..(LISTED_DIRECTORIES + 5) {
        put(&core, &format!("d{i}/f"), b"x");
    }
    let listings = ListingCache::default();
    for i in 0..(LISTED_DIRECTORIES + 5) {
        listings.listing(&core, &format!("d{i}")).unwrap();
    }
    let entries = listings.0.entries();
    assert!(entries.directories.len() <= LISTED_DIRECTORIES);
    assert!(entries.weight <= LISTED_ENTRIES);
    assert_eq!(
        entries.weight,
        entries.directories.values().map(|(_, w)| w).sum::<usize>()
    );
}
