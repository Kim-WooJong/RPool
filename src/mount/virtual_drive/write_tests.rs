//! Write-start durability barriers and deterministic save-work accounting.
use super::*;
use crate::mount::crash;
use crate::mount::virtual_tests::content;

fn cloud(drive: &VirtualDrive, path: &str) -> String {
    let event = Event {
        version: 1,
        device: "peer".into(),
        worker: "peer".into(),
        path: path.into(),
        parents: vec![],
        content: Some(content(b"")),
    };
    let id = event.id().unwrap();
    let mut s = drive.state.lock().unwrap();
    s.events.insert(id.clone(), event);
    s.save(&drive.root).unwrap();
    id
}

fn persisted(root: &Path) -> Vec<Option<Vec<u8>>> {
    [
        "namespace.json",
        "namespace.journal",
        "namespace.previous.json",
        "namespace.previous.journal",
    ]
    .iter()
    .map(|name| fs::read(root.join(name)).ok())
    .collect()
}

#[test]
fn repeated_begins_do_not_save_or_mutate_the_namespace() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let id = cloud(&drive, "doc");
    let visible = drive.visible_revision("doc").unwrap().unwrap();
    let first = drive.begin_intent("doc", Some(&visible)).unwrap();
    drive.discard_unsealed(&first).unwrap();
    let before = persisted(root.path());
    let generation = drive.state.lock().unwrap().generation;
    let revision = drive.state.lock().unwrap().revision();
    for _ in 0..32 {
        let intent = drive.begin_intent("doc", Some(&visible)).unwrap();
        assert_eq!(intent.parents, [id.as_str()]);
        drive.discard_unsealed(&intent).unwrap();
    }
    assert_eq!(drive.state.lock().unwrap().generation, generation);
    assert_eq!(drive.state.lock().unwrap().revision(), revision);
    assert_eq!(persisted(root.path()), before);
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().bases["doc"],
        [id]
    );
}

#[test]
fn failed_first_base_save_is_rolled_back_and_retried() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let id = cloud(&drive, "doc");
    let visible = drive.visible_revision("doc").unwrap().unwrap();
    let before = drive.state.lock().unwrap().generation;
    crash::arm("durable.before_persist");
    assert!(drive.begin_intent("doc", Some(&visible)).is_err());
    crash::disarm();
    assert!(!drive.state.lock().unwrap().bases.contains_key("doc"));
    assert_eq!(drive.state.lock().unwrap().generation, before);
    assert_eq!(fs::read_dir(root.path().join("spool")).unwrap().count(), 0);
    let intent = drive.begin_intent("doc", Some(&visible)).unwrap();
    assert_eq!(intent.parents, [id.as_str()]);
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().bases["doc"],
        [id]
    );
    drive.discard_unsealed(&intent).unwrap();
}

#[test]
fn begin_retries_a_dirty_base_left_by_failed_read_pin() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let id = cloud(&drive, "doc");
    let visible = drive.visible_revision("doc").unwrap().unwrap();
    crash::arm("durable.before_persist");
    assert!(drive.pin_read("doc", &visible).is_err());
    crash::disarm();
    assert_eq!(drive.state.lock().unwrap().bases["doc"], [id.as_str()]);
    assert!(!Namespace::load(root.path(), "tester")
        .unwrap()
        .bases
        .contains_key("doc"));
    let intent = drive.begin_intent("doc", Some(&visible)).unwrap();
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().bases["doc"],
        [id]
    );
    drive.discard_unsealed(&intent).unwrap();
}

#[test]
fn explicit_noop_save_still_rotates_recovery_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let intent = drive.begin_intent("new", None).unwrap();
    drive.discard_unsealed(&intent).unwrap();
    let before = fs::read(root.path().join("namespace.json")).unwrap();
    let generation = drive.state.lock().unwrap().generation;
    drive.state.lock().unwrap().save(root.path()).unwrap();
    assert_eq!(drive.state.lock().unwrap().generation, generation + 1);
    assert_eq!(
        fs::read(root.path().join("namespace.previous.json")).unwrap(),
        before
    );
    assert!(Namespace::load(root.path(), "tester")
        .unwrap()
        .bases
        .contains_key("new"));
}

#[test]
fn observe_read_retries_an_identical_but_unsaved_base() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let id = cloud(&drive, "doc");
    let visible = drive.visible_revision("doc").unwrap().unwrap();
    crash::arm("durable.before_persist");
    assert!(drive.pin_read("doc", &visible).is_err());
    crash::disarm();
    drive.observe_read("doc", &id).unwrap();
    assert_eq!(
        Namespace::load(root.path(), "tester").unwrap().bases["doc"],
        [id]
    );
    let generation = drive.state.lock().unwrap().generation;
    drive.observe_read("doc", visible.id()).unwrap();
    assert_eq!(drive.state.lock().unwrap().generation, generation);
}
