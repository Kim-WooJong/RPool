use super::super::tests::{fake_upload, fixture};
use super::*;

fn shared(worker: &str) -> (tempfile::TempDir, Workspace) {
    let (root, mut workspace) = fixture();
    workspace
        .configure_shared(Some("crypt:team"), Some(worker))
        .unwrap();
    (root, workspace)
}

fn archive(workspace: &mut Workspace, objects: &mut BTreeMap<String, Vec<u8>>) {
    workspace
        .sync_with(
            |path, id| {
                objects.insert(id.into(), fs::read(path)?);
                fake_upload(path, id)
            },
            false,
        )
        .unwrap();
    workspace.record_shared_changes().unwrap();
}

fn exchange(a: &mut Workspace, b: &mut Workspace) {
    let mut events = a.catalog.shared.as_ref().unwrap().events.clone();
    events.extend(b.catalog.shared.as_ref().unwrap().events.clone());
    a.catalog.shared.as_mut().unwrap().events = events.clone();
    b.catalog.shared.as_mut().unwrap().events = events;
}

fn apply(workspace: &mut Workspace, objects: &BTreeMap<String, Vec<u8>>) {
    let desired = shared_model::reduce(&workspace.catalog.shared.as_ref().unwrap().events).unwrap();
    workspace
        .materialize_shared(desired, |_, manifest, output, _| {
            let manifest: Manifest = read_json(manifest)?;
            fs::write(
                output,
                objects
                    .get(&manifest.archive_id)
                    .context("missing synthetic archive")?,
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn two_computers_offline_edits_converge_with_named_copies_and_no_echo_upload() {
    let (_ra, mut a) = shared("민수");
    let (_rb, mut b) = shared("지수");
    let mut objects = BTreeMap::new();
    fs::write(a.files.join("report.txt"), b"base").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    fs::write(a.files.join("report.txt"), b"alice edit").unwrap();
    fs::write(b.files.join("report.txt"), b"bob edit").unwrap();
    archive(&mut a, &mut objects);
    archive(&mut b, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut a, &objects);
    apply(&mut b, &objects);
    let fa = scan(&a.files).unwrap().0;
    let fb = scan(&b.files).unwrap().0;
    assert_eq!(fa.keys().collect::<Vec<_>>(), fb.keys().collect::<Vec<_>>());
    assert_eq!(fa.len(), 2);
    assert!(fa.keys().any(|p| p.contains("민수") || p.contains("지수")));
    let bytes: BTreeSet<_> = fa.values().map(|f| fs::read(&f.path).unwrap()).collect();
    assert_eq!(
        bytes,
        BTreeSet::from([b"alice edit".to_vec(), b"bob edit".to_vec()])
    );
    let before = a.catalog.shared.as_ref().unwrap().events.len();
    archive(&mut a, &mut objects);
    assert_eq!(a.catalog.shared.as_ref().unwrap().events.len(), before);
}

#[test]
fn delete_versus_offline_edit_preserves_editor_and_sequential_delete_propagates() {
    let (_ra, mut a) = shared("Alice");
    let (_rb, mut b) = shared("Bob");
    let mut objects = BTreeMap::new();
    fs::write(a.files.join("data.txt"), b"base").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    fs::remove_file(a.files.join("data.txt")).unwrap();
    fs::write(b.files.join("data.txt"), b"offline edit").unwrap();
    archive(&mut a, &mut objects);
    archive(&mut b, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut a, &objects);
    apply(&mut b, &objects);
    let files = scan(&a.files).unwrap().0;
    assert_eq!(files.len(), 1);
    let (name, file) = files.first_key_value().unwrap();
    assert!(name.contains("Bob"));
    assert_eq!(fs::read(&file.path).unwrap(), b"offline edit");
    fs::remove_file(&file.path).unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    assert!(scan(&b.files).unwrap().0.is_empty());
}

#[test]
fn cache_lease_and_missing_base_fail_closed() {
    let (root, mut a) = shared("Alice");
    assert!(a.shared_materialization_safe().unwrap());
    fs::write(a.metadata.join("mount-process.json"), b"{}").unwrap();
    assert!(!a.shared_materialization_safe().unwrap());
    fs::remove_file(a.metadata.join("mount-process.json")).unwrap();
    fs::create_dir_all(root.path().join("vfs-cache/nested")).unwrap();
    fs::write(root.path().join("vfs-cache/nested/pending"), b"unsaved").unwrap();
    assert!(!a.shared_materialization_safe().unwrap());
    assert_eq!(
        fs::read(root.path().join("vfs-cache/nested/pending")).unwrap(),
        b"unsaved"
    );
    assert!(a
        .configure_shared(Some("crypt:other"), Some("Alice"))
        .is_err());
    assert!(a.configure_shared(None, None).is_err());
}

#[test]
fn dirty_local_file_and_failed_download_are_never_overwritten() {
    let (_ra, mut a) = shared("Alice");
    let (_rb, mut b) = shared("Bob");
    let mut objects = BTreeMap::new();
    fs::write(a.files.join("file.txt"), b"cloud").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    let desired = shared_model::reduce(&b.catalog.shared.as_ref().unwrap().events).unwrap();
    assert!(b
        .materialize_shared(desired.clone(), |_, _, _, _| bail!("offline"))
        .is_err());
    assert!(!b.files.join("file.txt").exists());
    fs::write(b.files.join("file.txt"), b"unarchived").unwrap();
    assert!(b
        .materialize_shared(desired, |_, _, _, _| panic!("must not download"))
        .is_err());
    assert_eq!(fs::read(b.files.join("file.txt")).unwrap(), b"unarchived");
}

#[test]
fn interrupted_reconciliation_restores_old_bytes_without_overwriting_new() {
    let (_root, a) = shared("Alice");
    let dir = a.metadata.join("shared-recovery/apply-test/version-test");
    fs::create_dir_all(&dir).unwrap();
    atomic_json(&a.metadata.join("shared-apply.json"), &"apply-test").unwrap();
    atomic_json(&dir.join("origin.json"), &"report.txt").unwrap();
    fs::write(dir.join("content"), b"old").unwrap();
    fs::write(a.files.join("report.txt"), b"new").unwrap();
    a.recover_shared_apply().unwrap();
    assert_eq!(fs::read(a.files.join("report.txt")).unwrap(), b"new");
    assert!(scan(&a.files)
        .unwrap()
        .0
        .values()
        .any(|f| fs::read(&f.path).unwrap() == b"old"));
    assert!(!a.metadata.join("shared-apply.json").exists());
}

#[test]
fn recreation_descends_from_observed_remote_deletion() {
    let (_ra, mut a) = shared("Alice");
    let (_rb, mut b) = shared("Bob");
    let mut objects = BTreeMap::new();
    fs::write(a.files.join("file.txt"), b"base").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    fs::remove_file(a.files.join("file.txt")).unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    assert!(!b.files.join("file.txt").exists());
    fs::write(b.files.join("file.txt"), b"recreated").unwrap();
    archive(&mut b, &mut objects);
    exchange(&mut a, &mut b);
    let desired = shared_model::reduce(&a.catalog.shared.as_ref().unwrap().events).unwrap();
    assert_eq!(desired.len(), 1);
    assert!(desired.contains_key("file.txt"));
    let event = &desired["file.txt"].event;
    assert!(!event.parents.is_empty());
    assert!(event
        .parents
        .iter()
        .all(|id| a.catalog.shared.as_ref().unwrap().events[id]
            .content
            .is_none()));
}

#[test]
fn long_unicode_conflict_names_remain_portable() {
    let (_ra, mut a) = shared("한글작업자");
    let (_rb, mut b) = shared("Other");
    let name = format!("{}.txt", "긴".repeat(80));
    let mut objects = BTreeMap::new();
    fs::write(a.files.join(&name), b"one").unwrap();
    fs::write(b.files.join(&name), b"two").unwrap();
    archive(&mut a, &mut objects);
    archive(&mut b, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut a, &objects);
    let files = scan(&a.files).unwrap().0;
    assert_eq!(files.len(), 2);
    assert!(files.keys().all(|p| p.len() <= 255));
}

#[test]
fn remote_directory_to_file_transition_preserves_local_backup() {
    let (_ra, mut a) = shared("Alice");
    let (_rb, mut b) = shared("Bob");
    let mut objects = BTreeMap::new();
    fs::create_dir(a.files.join("folder")).unwrap();
    fs::write(a.files.join("folder/child"), b"nested").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    fs::remove_file(a.files.join("folder/child")).unwrap();
    fs::remove_dir(a.files.join("folder")).unwrap();
    fs::write(a.files.join("folder"), b"now a file").unwrap();
    archive(&mut a, &mut objects);
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    assert_eq!(fs::read(b.files.join("folder")).unwrap(), b"now a file");
}

#[test]
fn same_content_relocated_manifest_publishes_successor_and_keeps_history() {
    let (_root, mut a) = shared("Alice");
    let (_other, mut b) = shared("Bob");
    let mut objects = BTreeMap::new();
    fs::write(a.files.join("report.txt"), b"abc").unwrap();
    archive(&mut a, &mut objects);
    let before = a.catalog.shared.as_ref().unwrap().events.clone();
    let old = a.catalog.entries["report.txt"].manifest.clone();
    let mut relocated: Manifest = read_json(&a.metadata.join("archives").join(&old)).unwrap();
    relocated.archive_id = "relocated".into();
    objects.insert("relocated".into(), b"abc".to_vec());
    let name = format!(
        "{}.json",
        crate::manifest::manifest_fingerprint(&relocated).unwrap()
    );
    atomic_json(&a.metadata.join("archives").join(&name), &relocated).unwrap();
    a.catalog.entries.get_mut("report.txt").unwrap().manifest = name.clone();
    a.record_shared_changes().unwrap();
    assert_eq!(
        a.catalog.shared.as_ref().unwrap().events.len(),
        before.len() + 1
    );
    for (id, event) in before {
        assert_eq!(
            a.catalog.shared.as_ref().unwrap().events[&id].id().unwrap(),
            event.id().unwrap()
        );
    }
    exchange(&mut a, &mut b);
    apply(&mut b, &objects);
    assert_eq!(b.catalog.entries["report.txt"].manifest, name);
    assert_eq!(fs::read(b.files.join("report.txt")).unwrap(), b"abc");
    assert!(a.metadata.join("archives").join(old).exists());
}
