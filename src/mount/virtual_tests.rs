use super::namespace::{Intent, Namespace};
use super::shared_model::{Content, Event};
use super::virtual_drive::{fixture, recover_spool, Revision};
use crate::prelude::*;

#[test]
fn online_mount_refuses_legacy_or_nonempty_workspace_without_changing_files() {
    for replica in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("keep-local-write");
        fs::write(&file, b"not uploaded").unwrap();
        if replica {
            fs::create_dir(temp.path().join(".rpool")).unwrap();
            fs::write(temp.path().join(".rpool/catalog.json"), b"legacy").unwrap();
        }
        let error = super::virtual_drive::VirtualDrive::open(
            "unused-rclone",
            "unused-pool",
            temp.path(),
            "pc",
            None,
            8,
            false,
            false,
            false,
        )
        .err()
        .expect("must not convert existing data");
        assert!(error.to_string().contains(if replica {
            "never silently converted"
        } else {
            "initially be empty"
        }));
        assert_eq!(fs::read(file).unwrap(), b"not uploaded");
        assert!(!temp.path().join("virtual.json").exists());
        assert!(!temp.path().join("virtual.lock").exists());
    }
}

pub(super) fn content(bytes: &[u8]) -> Content {
    let hash = blake3::hash(bytes).to_hex().to_string();
    let shards = vec![Shard {
        index: 0,
        offset: 0,
        size: bytes.len() as u64,
        remote: "offline:".into(),
        object: "offline:object".into(),
        blake3: hash.clone(),
        kind: ShardKind::Data,
        group: 0,
        slot: 0,
    }];
    Content {
        hash,
        size: bytes.len() as u64,
        manifest: Manifest {
            version: 1,
            archive_id: "test".into(),
            original_name: "file".into(),
            original_size: bytes.len() as u64,
            shard_size: 4,
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v1(&shards),
            coding: None,
            shards,
        },
    }
}
fn add(
    s: &mut Namespace,
    path: &str,
    bytes: Option<&[u8]>,
    parents: Vec<String>,
    worker: &str,
) -> String {
    let event = Event {
        version: 1,
        device: worker.into(),
        worker: worker.into(),
        path: path.into(),
        parents,
        content: bytes.map(content),
    };
    event.validate().unwrap();
    let id = event.id().unwrap();
    s.events.insert(id.clone(), event);
    id
}
fn write(d: &super::virtual_drive::VirtualDrive, path: &str, bytes: &[u8]) -> Intent {
    let i = d.begin(path).unwrap();
    fs::write(d.spool_path(&i), bytes).unwrap();
    d.seal(i.clone()).unwrap();
    d.state.lock().unwrap().pending.last().unwrap().clone()
}
#[test]
fn incremental_base_is_captured_ancestry_not_new_remote_head() {
    fn archive(state: &mut Namespace, event_id: String, receipt: &str) -> String {
        let mut event = state.events.remove(&event_id).unwrap();
        event.content.as_mut().unwrap().manifest.archive_id = format!("virtual-{receipt}");
        let id = event.id().unwrap();
        state.events.insert(id.clone(), event);
        id
    }
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    let mut state = d.state.lock().unwrap();
    let original = add(&mut state, "file", Some(b"original"), vec![], "a");
    let original = archive(&mut state, original, "original-local-write");
    let latest = add(
        &mut state,
        "file",
        Some(b"remote"),
        vec![original.clone()],
        "b",
    );
    let latest = archive(&mut state, latest, "earlier-local-write");
    state
        .committed_intents
        .insert("original-local-write".into(), original.clone());
    let mut intent = Intent {
        id: "pending".into(),
        path: "file".into(),
        event_path: "file".into(),
        parents: vec![original.clone()],
        spool: None,
        size: 0,
        hash: String::new(),
        depends_on: None,
        checkpoint_base: None,
        checkpoint_serial: None,
    };
    assert_eq!(
        state.upload_base(&intent).unwrap().unwrap().hash,
        content(b"original").hash
    );
    intent.parents = vec![latest.clone()];
    assert!(state.upload_base(&intent).unwrap().is_none()); // no local retention receipt
    intent.parents = vec![original];
    intent.parents.push(latest.clone());
    assert!(state.upload_base(&intent).unwrap().is_none());
    intent.depends_on = Some("earlier-local-write".into());
    assert!(state.upload_base(&intent).is_err());
    state
        .committed_intents
        .insert("earlier-local-write".into(), latest);
    assert_eq!(
        state.upload_base(&intent).unwrap().unwrap().hash,
        content(b"remote").hash
    );
    intent.event_path = "other".into();
    assert!(state.upload_base(&intent).unwrap().is_none());
}
#[test]
fn metadata_only_rename_does_not_grant_imported_archive_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let mut d = fixture(temp.path());
    d.pool_sync_roots = vec!["crypt:pool-sync".into()];
    {
        let mut state = d.state.lock().unwrap();
        state.version = 6;
        add(
            &mut state,
            "external",
            Some(b"imported"),
            vec![],
            "other-pc",
        );
    }
    d.rename_file("external", "renamed").unwrap();
    let revision = d.view().unwrap().remove("renamed").unwrap();
    d.pin_read("renamed", &revision).unwrap();
    let intent = write(&d, "renamed", b"modified");
    let state = d.state.lock().unwrap();
    assert!(intent
        .parents
        .iter()
        .any(|parent| state.committed_intents.values().any(|id| id == parent)));
    assert!(state.upload_base(&intent).unwrap().is_none());
}
#[test]
fn repeated_committed_moves_preserve_payload_and_upload_provenance() {
    for directory in [false, true] {
        for local_upload in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut d = fixture(temp.path());
            // Any accidental hydration or payload upload fails immediately.
            d.rclone = temp
                .path()
                .join("no-rclone-here")
                .to_string_lossy()
                .into_owned();
            d.pool_sync_roots = vec!["crypt:pool-sync".into()];
            d.state.lock().unwrap().version = 6;
            let names = if directory { vec!["a", "b"] } else { vec![""] };
            let mut expected = BTreeMap::new();
            for name in &names {
                let path = if directory {
                    format!("start/{name}")
                } else {
                    "start".into()
                };
                let value = if local_upload {
                    let intent = write(&d, &path, b"committed payload");
                    let mut value = content(b"committed payload");
                    value.manifest.archive_id = format!("virtual-{}", intent.id);
                    d.commit_uploaded(&intent, Some(value.clone())).unwrap();
                    value
                } else {
                    let mut state = d.state.lock().unwrap();
                    let id = add(
                        &mut state,
                        &path,
                        Some(b"imported payload"),
                        vec![],
                        "other-pc",
                    );
                    state.events[&id].content.clone().unwrap()
                };
                expected.insert(*name, serde_json::to_vec(&value.manifest).unwrap());
            }
            for (from, to) in [("start", "middle"), ("middle", "finish")] {
                if directory {
                    d.rename_directory(from, to).unwrap();
                } else {
                    d.rename_file(from, to).unwrap();
                }
                assert!(d.state.lock().unwrap().pending.is_empty());
                assert!(d.view().unwrap().keys().all(|p| !p.starts_with(from)));
            }
            for name in &names {
                let path = if directory {
                    format!("finish/{name}")
                } else {
                    "finish".into()
                };
                let revision = d.view().unwrap().remove(&path).unwrap();
                let Revision::Cloud { content: value, .. } = &revision else {
                    panic!("move hydrated content")
                };
                assert_eq!(serde_json::to_vec(&value.manifest).unwrap(), expected[name]);
                d.pin_read(&path, &revision).unwrap();
                let mut intent = d.begin_observed(&path, Some(&revision)).unwrap();
                let mut state = d.state.lock().unwrap();
                let base = state.upload_base(&intent).unwrap();
                assert_eq!(base.is_some(), local_upload);
                if local_upload {
                    assert_eq!(
                        serde_json::to_vec(&base.unwrap().manifest).unwrap(),
                        expected[name]
                    );
                    // Even a matching trusted archive name cannot substitute a
                    // different manifest for the original upload receipt.
                    let mut forged = state.events[&intent.parents[0]].clone();
                    forged.content.as_mut().unwrap().manifest.original_name = "different".into();
                    let id = forged.id().unwrap();
                    state.events.insert(id.clone(), forged);
                    intent.parents = vec![id];
                    assert!(state.upload_base(&intent).unwrap().is_none());
                    state.events.remove(&intent.parents[0]);
                }
            }
        }
    }
}
#[test]
fn durable_pending_survives_restart_and_recovery_does_not_rewrite_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    let i = write(&d, "folder/file", b"draft");
    let before = fs::read(temp.path().join("namespace.json")).unwrap();
    let s = Namespace::load(temp.path(), "restarted").unwrap();
    assert_eq!(s.pending[0].hash, i.hash);
    assert_eq!(s.visible_logical_used().unwrap(), 5);
    let files = recover_spool(temp.path()).unwrap();
    assert_eq!(fs::read(&files[0]).unwrap(), b"draft");
    assert_eq!(
        before,
        fs::read(temp.path().join("namespace.json")).unwrap()
    );
    fs::write(temp.path().join("namespace.json"), b"broken").unwrap();
    assert!(Namespace::load(temp.path(), "restarted").is_err());
    assert_eq!(recover_spool(temp.path()).unwrap(), files);
}
#[test]
fn incomplete_write_is_not_listed_and_exports_as_partial() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    let i = d.begin("file").unwrap();
    fs::write(d.spool_path(&i), b"partial").unwrap();
    assert!(d.view().unwrap().is_empty());
    let paths = recover_spool(temp.path()).unwrap();
    assert!(paths[0].to_string_lossy().ends_with(".partial.bin"));
}
#[test]
fn pending_case_and_file_directory_collisions_are_rejected_before_ack() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    write(&d, "A", b"one");
    for name in ["a", "A/child"] {
        let i = d.begin(name).unwrap();
        fs::write(d.spool_path(&i), b"two").unwrap();
        assert!(d.seal(i).is_err());
    }
    assert_eq!(d.state.lock().unwrap().pending.len(), 1);
}
#[test]
fn pinned_revision_survives_remote_replacement_and_usage_excludes_cache_pins() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    let first = add(
        &mut d.state.lock().unwrap(),
        "file",
        Some(b"old"),
        vec![],
        "a",
    );
    let revision = d.view().unwrap()["file"].clone();
    d.pin_read("file", &revision).unwrap();
    add(
        &mut d.state.lock().unwrap(),
        "file",
        Some(b"next"),
        vec![first.clone()],
        "b",
    );
    let view = d.view().unwrap();
    assert_eq!(view["file"].id(), first);
    assert_eq!(view.len(), 2);
    assert_eq!(d.state.lock().unwrap().logical_used().unwrap(), 4);
    let edit = d.begin("file").unwrap();
    assert_eq!(edit.parents, vec![first]);
}
#[test]
fn delete_recreate_parents_tombstone_and_publication_is_causal() {
    let mut s = Namespace::create("test").unwrap();
    let a = add(&mut s, "file", Some(b"one"), vec![], "a");
    let b = add(&mut s, "file", None, vec![a.clone()], "b");
    assert_eq!(s.base("file").unwrap(), vec![b.clone()]);
    let c = add(&mut s, "file", Some(b"new"), vec![b.clone()], "c");
    assert_eq!(
        s.unpublished_ordered()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec![a, b, c]
    );
    assert_eq!(s.logical_used().unwrap(), 3);
}
#[test]
fn concurrent_modification_and_deletion_preserve_edit_and_conflict() {
    let mut s = Namespace::create("test").unwrap();
    let a = add(&mut s, "file", Some(b"old"), vec![], "a");
    add(&mut s, "file", None, vec![a.clone()], "b");
    add(&mut s, "file", Some(b"new"), vec![a], "c");
    let resolved = s.resolved().unwrap();
    assert_eq!(
        resolved
            .values()
            .filter(|r| r.event.content.is_some())
            .count(),
        1
    );
    assert_eq!(s.logical_used().unwrap(), 3);
}
#[test]
fn sealed_local_revision_reads_its_own_bytes_without_cloud_access() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    write(&d, "file", b"draft");
    let view = d.view().unwrap();
    assert!(matches!(view["file"], Revision::Local { .. }));
    assert_eq!(d.read(&view["file"], 1, 3).unwrap(), b"raf");
}

#[test]
fn local_save_and_delete_do_not_revert_after_upload() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    add(
        &mut d.state.lock().unwrap(),
        "file",
        Some(b"old"),
        vec![],
        "a",
    );
    let old = d.view().unwrap()["file"].clone();
    d.pin_read("file", &old).unwrap();
    let intent = write(&d, "file", b"new");
    d.commit_uploaded(&intent, Some(content(b"new"))).unwrap();
    let view = d.view().unwrap();
    assert_eq!(view.len(), 1);
    match &view["file"] {
        Revision::Cloud { content: c, .. } => assert_eq!(c.hash, content(b"new").hash),
        _ => panic!(),
    }
    d.delete("file").unwrap();
    let deletion = d.state.lock().unwrap().pending[0].clone();
    d.commit_uploaded(&deletion, None).unwrap();
    assert!(d.view().unwrap().is_empty());
}
#[test]
fn atomic_save_move_preserves_receipt_and_deletes_pending_source_causally() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    write(&d, "temp", b"new");
    d.rename_file("temp", "file").unwrap();
    let intents = d.state.lock().unwrap().pending.clone();
    assert_eq!(intents.len(), 3);
    assert!(d
        .spool_path(&intents[1])
        .parent()
        .unwrap()
        .join("intent.json")
        .exists());
    for i in intents {
        let c = i.spool.as_ref().map(|_| content(b"new"));
        d.commit_uploaded(&i, c).unwrap();
    }
    let view = d.view().unwrap();
    assert_eq!(view.keys().cloned().collect::<Vec<_>>(), vec!["file"]);
}
#[test]
fn conflicting_directory_spelling_is_rejected_before_ack() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    write(&d, "Dir/a", b"one");
    let i = d.begin("dir/b").unwrap();
    fs::write(d.spool_path(&i), b"two").unwrap();
    assert!(d.seal(i).is_err());
    assert_eq!(d.state.lock().unwrap().pending.len(), 1);
}

#[test]
fn folder_move_checkpoints_all_children_together_and_preserves_empty_directories() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    write(&d, "folder/a", b"new");
    write(&d, "folder/b", b"new");
    d.state
        .lock()
        .unwrap()
        .directories
        .insert("folder/empty".into());
    d.rename_directory("folder", "moved").unwrap();
    assert_eq!(
        d.view().unwrap().keys().cloned().collect::<Vec<_>>(),
        vec!["moved/a", "moved/b"]
    );
    assert!(d.state.lock().unwrap().directories.contains("moved/empty"));
    let intents = d.state.lock().unwrap().pending.clone();
    for i in intents {
        let c = i.spool.as_ref().map(|_| content(b"new"));
        d.commit_uploaded(&i, c).unwrap();
    }
    assert_eq!(
        d.view().unwrap().keys().cloned().collect::<Vec<_>>(),
        vec!["moved/a", "moved/b"]
    );
    let restored = Namespace::load(temp.path(), "tester").unwrap();
    assert_eq!(restored.logical_used().unwrap(), 6);
}

#[test]
fn directory_rename_back_descends_from_tombstones_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    let i = write(&d, "folder/a", b"new");
    d.commit_uploaded(&i, Some(content(b"new"))).unwrap();
    d.rename_directory("folder", "moved").unwrap();
    d.rename_directory("moved", "folder").unwrap();
    let s = Namespace::load(temp.path(), "tester").unwrap();
    assert_eq!(
        s.resolved().unwrap().keys().cloned().collect::<Vec<_>>(),
        vec!["folder/a"]
    );
}

#[test]
fn quota_never_reuses_same_size_committed_replacement_or_expired_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    {
        let mut state = drive.state.lock().unwrap();
        add(&mut state, "file", Some(b"old"), vec![], "a");
    }
    let state = drive.state.lock().unwrap();
    let used = state.visible_logical_used().unwrap();
    *drive.capacity.lock().unwrap() = Some(super::capacity::CapacityStatus {
        eligible: vec!["offline:".into()],
        logical_used: used,
        additional_estimate: 100,
        namespace_event_ids: state.events.keys().cloned().collect(),
        observed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        ..Default::default()
    });
    drop(state);
    assert_eq!(drive.quota(), Some((3, Some(103))));
    {
        let mut state = drive.state.lock().unwrap();
        let parents = state.base("file").unwrap();
        add(&mut state, "file", Some(b"new"), parents, "b");
    }
    assert_eq!(drive.quota(), Some((3, Some(3))));
    drive
        .capacity
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .observed_unix = 0;
    assert_eq!(drive.quota(), None);
    *drive.capacity.lock().unwrap() = None;
    assert_eq!(drive.quota(), None);
}
