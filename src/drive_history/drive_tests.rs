//! Drive history on a real (local, cloud-less) v6 workspace: the restore and
//! rollback revisions are ordinary v6 events an older RPool reads.
use super::apply::apply;
use super::graph::History;
use super::model::{ChangeAction, Retention, VersionKind};
use super::source_v6::fixture::{add, event};
use crate::mount::history_bridge::{self as bridge, testing, Event};
use crate::prelude::*;

fn history(drive: &bridge::VirtualDrive, times: &BTreeMap<String, u64>, now: u64) -> History {
    let (events, unpublished) = bridge::v6_events(drive);
    super::source_v6::build(events, times, &unpublished, now, BTreeSet::new()).unwrap()
}

/// Exactly the v1 event fields older RPool knows.
fn assert_plain_v1(event: &Event) {
    let value = serde_json::to_value(event).unwrap();
    let keys: BTreeSet<_> = value.as_object().unwrap().keys().cloned().collect();
    let expected: BTreeSet<String> = ["version", "worker", "device", "path", "parents", "content"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(keys, expected);
    assert_eq!(event.version, 1);
    event.validate().unwrap();
}

#[test]
fn trash_restore_publishes_a_normal_event_descending_from_the_deletion() {
    let root = tempfile::tempdir().unwrap();
    let mut events = BTreeMap::new();
    let a1 = add(&mut events, event("PC-B", "Docs/a.txt", &[], Some("alpha")));
    let a2 = add(&mut events, event("PC-B", "Docs/a.txt", &[&a1], None));
    let manifest = events[&a1].content.clone().unwrap().manifest;
    let drive = testing::v6_drive(root.path(), events);
    let times: BTreeMap<_, _> = [(a1.clone(), 100), (a2.clone(), 200)].into();
    let h = history(&drive, &times, 1000);
    let files = super::trash::files(&h, &Retention::default(), 1000).unwrap();
    assert_eq!(files.len(), 1);
    let actions =
        super::trash::restore_actions(&h, &files, std::slice::from_ref(&a2), None, None).unwrap();
    let applied = apply(&drive, &h, &actions).unwrap();
    assert!(!applied.published, "no cloud in this test");
    let (all, unpublished) = bridge::v6_events(&drive);
    assert_eq!(unpublished.len(), 1);
    let restored = &all[unpublished.iter().next().unwrap()];
    assert_plain_v1(restored);
    assert_eq!(restored.parents, [a2]);
    assert_eq!(
        restored.content.as_ref().unwrap().manifest.archive_id,
        manifest.archive_id
    );
    assert!(drive.view().unwrap().contains_key("Docs/a.txt"));
    // Another PC's projection of all events shows the file again.
    let projection = crate::mount::peer_projection::project(&all).unwrap();
    assert!(projection.files.contains_key("Docs/a.txt"));
    let h = history(&drive, &times, 1000);
    assert!(super::trash::list(&h, &Retention::default(), 1000)
        .unwrap()
        .is_empty());
    let versions = super::versions::list(&h, "Docs/a.txt").unwrap();
    assert_eq!(versions[0].kind, VersionKind::Restored);
    assert!(versions[0].current && versions[0].time_unix == Some(1000));
}

#[test]
fn version_copy_rollback_and_rollback_of_rollback_on_a_workspace() {
    let root = tempfile::tempdir().unwrap();
    let mut events = BTreeMap::new();
    let f1 = add(&mut events, event("pc", "f.txt", &[], Some("one")));
    let f2 = add(&mut events, event("pc", "f.txt", &[&f1], Some("two")));
    let g1 = add(&mut events, event("pc", "g.txt", &[], Some("g")));
    let drive = testing::v6_drive(root.path(), events);
    let times: BTreeMap<_, _> = [(f1.clone(), 100), (f2.clone(), 200), (g1.clone(), 300)].into();
    let retention = Retention::default();

    // Restore an old version as a copy: a new file, the original unchanged.
    let h = history(&drive, &times, 1000);
    let copy = super::versions::restore_action(&h, "f.txt", &f1, true).unwrap();
    apply(&drive, &h, &[copy]).unwrap();
    assert!(drive.view().unwrap().contains_key("f (restored).txt"));

    // Roll the drive back to 150: f.txt reverts to "one", g.txt goes to the trash.
    let h = history(&drive, &times, 1000);
    let plan = super::rollback::plan(&h, "p", "", 150).unwrap();
    let summary: Vec<_> = plan
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.action))
        .collect();
    assert_eq!(
        summary,
        [
            ("/f (restored).txt", ChangeAction::Remove),
            ("/f.txt", ChangeAction::Revert),
            ("/g.txt", ChangeAction::Remove)
        ]
    );
    apply(&drive, &h, &super::rollback::actions(&plan).unwrap()).unwrap();
    testing::commit_pending_deletions(&drive);
    let h = history(&drive, &times, 1000);
    assert!(super::rollback::plan(&h, "p", "", 1000)
        .unwrap()
        .changes
        .is_empty());
    let trash = super::trash::list(&h, &retention, 1000).unwrap();
    assert!(trash.iter().any(|e| e.path == "/g.txt"));

    // Undo the rollback: back to 500 (after the copy... the copy was at 1000).
    let undo = super::rollback::plan(&h, "p", "", 500).unwrap();
    let summary: Vec<_> = undo
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.action, c.revision.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            ("/f.txt", ChangeAction::Revert, f2.clone()),
            ("/g.txt", ChangeAction::Undelete, g1.clone())
        ]
    );
    apply(&drive, &h, &super::rollback::actions(&undo).unwrap()).unwrap();
    let view = drive.view().unwrap();
    let content = |path: &str| match &view[path] {
        crate::mount::history_bridge::Revision::Cloud { content, .. } => content.hash.clone(),
        _ => panic!("not committed"),
    };
    let (all, _) = bridge::v6_events(&drive);
    assert_eq!(content("f.txt"), all[&f2].content.as_ref().unwrap().hash);
    assert_eq!(content("g.txt"), all[&g1].content.as_ref().unwrap().hash);
    for event in all.values() {
        assert_plain_v1(event);
    }
}

#[test]
fn pending_local_writes_block_history_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut events = BTreeMap::new();
    let f1 = add(&mut events, event("pc", "f.txt", &[], Some("one")));
    let f2 = add(&mut events, event("pc", "f.txt", &[&f1], Some("two")));
    let drive = testing::v6_drive(root.path(), events);
    drive.delete("f.txt").unwrap(); // queued, not yet uploaded
    let h = history(&drive, &BTreeMap::new(), 1000);
    let action = super::versions::restore_action(&h, "f.txt", &f1, false).unwrap();
    let error = apply(&drive, &h, &[action]).unwrap_err();
    assert!(format!("{error}").contains("not yet uploaded"));
    let _ = f2;
}
