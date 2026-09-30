//! Pool-sync (v6) traces: peers' revisions arrive as namespace events while
//! files are open. Handles never mix revisions, edits descend from the bytes
//! they were built on, and concurrent edits become preserved conflicts.
//! Peer contents are empty so they read without rclone; own revisions stay
//! readable from the spool.
use super::*;
use crate::mount::shared_model::Event;
use crate::mount::virtual_drive::{fixture, Revision, VirtualDrive};
use crate::mount::virtual_tests::content;
use crate::prelude::*;

const W: Access = Access::Write {
    truncate: false,
    append: false,
};
const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

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
fn read(core: &FsCore, handle: HandleId) -> Vec<u8> {
    core.read_at(handle, 0, 1 << 20).unwrap()
}
/// Commits every pending intent, as `sync` does after uploading.
fn commit_all(drive: &VirtualDrive) -> Vec<String> {
    let pending = drive.state.lock().unwrap().pending.clone();
    let mut events = Vec::new();
    for intent in pending {
        let bytes = intent
            .spool
            .as_ref()
            .map(|_| fs::read(drive.spool_path(&intent)).unwrap());
        drive
            .commit_uploaded(&intent, bytes.as_deref().map(content))
            .unwrap();
        events.push(drive.state.lock().unwrap().committed_intents[&intent.id].clone());
    }
    events
}
/// A peer PC's edit of `path` (empty content) descending from `parents`.
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
fn last_pending(drive: &VirtualDrive) -> crate::mount::namespace::Intent {
    drive.state.lock().unwrap().pending.last().unwrap().clone()
}
fn visible(drive: &VirtualDrive, path: &str) -> Option<Revision> {
    drive.view().unwrap().get(path).cloned()
}

#[test]
fn reopening_a_file_this_workspace_wrote_never_trips_a_remount_fence() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"one");
    put(&core, "a", b"two");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, reader), b"two");
    let writer = core.open("a", W, false, false).unwrap();
    core.write_at(writer, 3, b"!").unwrap();
    core.release(writer).unwrap();
    let again = core.open("a", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, again), b"two!");
    assert_eq!(
        read(&core, reader),
        b"two",
        "the older snapshot is unchanged"
    );
}

#[test]
fn open_handles_keep_their_base_when_a_peer_revision_arrives() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"mine");
    let attached = core.open("a", W, false, false).unwrap();
    let snapshot = core.open("a", Access::Read, false, false).unwrap();
    let mine = commit_all(&core.drive)[0].clone();
    let peer = peer_edit(&core.drive, "a", vec![mine], false);
    assert_eq!(visible(&core.drive, "a").unwrap().id(), peer);
    assert_eq!(read(&core, attached), b"mine");
    assert_eq!(read(&core, snapshot), b"mine");
    assert_eq!(core.stat(attached).unwrap().size, 4);
    // A second attached open joins the shared base instead of the peer's.
    let joined = core.open("a", W, false, false).unwrap();
    assert_eq!(read(&core, joined), b"mine");
    for handle in [attached, snapshot, joined] {
        core.release(handle).unwrap();
    }
    // With every handle closed, new opens see the peer revision.
    let fresh = core.open("a", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, fresh), b"");
    assert_eq!(core.lookup("a").unwrap().tag, peer);
}

#[test]
fn a_partial_edit_after_a_peer_edit_becomes_a_preserved_conflict() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "doc", b"shared text");
    let writer = core.open("doc", W, false, false).unwrap();
    let base = commit_all(&core.drive)[0].clone();
    let peer = peer_edit(&core.drive, "doc", vec![base.clone()], false);
    core.write_at(writer, 0, b"SHARED").unwrap();
    core.release(writer).unwrap();
    let ours = last_pending(&core.drive);
    assert_eq!(
        ours.parents,
        vec![base],
        "descends from the bytes it edited"
    );
    assert_eq!(ours.depends_on, None);
    commit_all(&core.drive);
    let view = core.drive.view().unwrap();
    // v6 keeps the common original at the path and names both edits.
    let copies: Vec<&String> = view.keys().filter(|p| p.starts_with("doc")).collect();
    assert_eq!(copies.len(), 3, "original plus both edits: {copies:?}");
    assert!(view.values().any(|r| r.id() == peer));
    assert!(
        view.values().any(|r| r.size() == 11),
        "our edit kept its bytes"
    );
}

#[test]
fn a_truncating_save_descends_from_what_was_read_or_else_from_the_visible_revision() {
    // Read, close, peer edit, then an O_TRUNC save: a sibling of the peer.
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "note", b"draft");
    let reader = core.open("note", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, reader), b"draft");
    core.release(reader).unwrap();
    let base = commit_all(&core.drive)[0].clone();
    let peer = peer_edit(&core.drive, "note", vec![base.clone()], false);
    put(&core, "note", b"saved");
    assert_eq!(last_pending(&core.drive).parents, vec![base]);
    // A save without any read in this session follows the visible revision.
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    let origin = peer_edit(&core.drive, "fresh", vec![], false);
    let next = peer_edit(&core.drive, "fresh", vec![origin], false);
    put(&core, "fresh", b"new");
    assert_eq!(last_pending(&core.drive).parents, vec![next]);
    drop(peer);
}

#[test]
fn own_successive_saves_never_conflict_with_each_other() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"1");
    put(&core, "a", b"2");
    let first = core.drive.state.lock().unwrap().pending[0].id.clone();
    assert_eq!(last_pending(&core.drive).depends_on, Some(first));
    commit_all(&core.drive);
    put(&core, "a", b"3");
    let third = last_pending(&core.drive);
    assert_eq!(third.depends_on, None);
    assert_eq!(third.parents.len(), 1, "continues the committed save");
    commit_all(&core.drive);
    let view = core.drive.view().unwrap();
    assert_eq!(view.keys().collect::<Vec<_>>(), ["a"], "no self-conflict");
    assert_eq!(view["a"].size(), 1);
}

#[test]
fn deleting_after_a_peer_edit_keeps_the_peer_revision() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"old");
    let reader = core.open("a", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, reader), b"old");
    core.release(reader).unwrap();
    let base = commit_all(&core.drive)[0].clone();
    let peer = peer_edit(&core.drive, "a", vec![base.clone()], false);
    core.delete("a").unwrap();
    assert_eq!(last_pending(&core.drive).parents, vec![base]);
    commit_all(&core.drive);
    let view = core.drive.view().unwrap();
    assert!(
        view.values().any(|r| r.id() == peer),
        "peer edit survives our delete"
    );
}

#[test]
fn a_peer_delete_while_open_does_not_lose_our_later_edit() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "a", b"kept");
    let writer = core.open("a", W, false, false).unwrap();
    let base = commit_all(&core.drive)[0].clone();
    peer_edit(&core.drive, "a", vec![base.clone()], true);
    core.write_at(writer, 4, b"+").unwrap();
    core.release(writer).unwrap();
    assert_eq!(last_pending(&core.drive).parents, vec![base]);
    commit_all(&core.drive);
    let view = core.drive.view().unwrap();
    assert!(
        view.values().any(|r| r.size() == 5),
        "our edit survives the peer delete: {:?}",
        view.keys()
    );
}

#[test]
fn rename_keeps_bytes_and_later_edits_continue_the_moved_file() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    put(&core, "from", b"data");
    let handle = core.open("from", W, false, false).unwrap();
    core.rename("from", "to").unwrap();
    assert_eq!(read(&core, handle), b"data");
    core.write_at(handle, 4, b"!").unwrap();
    core.release(handle).unwrap();
    let edit = last_pending(&core.drive);
    assert_eq!(edit.path, "to");
    assert!(edit.depends_on.is_some(), "continues the moved copy");
    let reader = core.open("to", Access::Read, false, false).unwrap();
    assert_eq!(read(&core, reader), b"data!");
}

#[test]
fn conflict_copies_are_listed_with_their_own_identities() {
    let root = tempfile::tempdir().unwrap();
    let core = peer_core(root.path());
    let origin = peer_edit(&core.drive, "x", vec![], false);
    peer_edit(&core.drive, "x", vec![origin.clone()], false);
    let mut other = Event {
        version: 1,
        device: "third".into(),
        worker: "third".into(),
        path: "x".into(),
        parents: vec![origin],
        content: Some(content(b"")),
    };
    other.worker = "third".into();
    let id = other.id().unwrap();
    core.drive.state.lock().unwrap().events.insert(id, other);
    let names: Vec<String> = core.readdir("").unwrap().into_iter().map(|e| e.0).collect();
    assert!(names.len() >= 2, "{names:?}");
    let ids: std::collections::BTreeSet<_> = names
        .iter()
        .map(|n| {
            let h = core.open(n, Access::Read, false, false).unwrap();
            let id = core.stat(h).unwrap().id;
            core.release(h).unwrap();
            id
        })
        .collect();
    assert_eq!(ids.len(), names.len());
}
