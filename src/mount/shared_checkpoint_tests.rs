use super::super::virtual_drive::{fixture, Revision};
use super::*;
use std::cell::RefCell;
fn id(n: u64) -> String {
    format!("{n:064x}")
}
fn pointer(c: &Checkpoint) -> Pointer {
    Pointer {
        version: 5,
        owner: c.owner.clone(),
        serial: c.serial,
        hash: blake3::hash(&serde_json::to_vec(c).unwrap())
            .to_hex()
            .to_string(),
    }
}
fn setup(root: &Path) -> (VirtualDrive, Checkpoint) {
    let mut d = fixture(root);
    d.bounded_shared = true;
    let c = Checkpoint::empty(&d.state.lock().unwrap().device);
    d.install_checkpoint(&pointer(&c), &c).unwrap();
    (d, c)
}
fn write(d: &VirtualDrive, name: &str, bytes: &[u8]) -> Intent {
    let i = d.begin(name).unwrap();
    fs::write(d.spool_path(&i), bytes).unwrap();
    d.seal(i).unwrap();
    d.state.lock().unwrap().pending.last().unwrap().clone()
}
fn proposal(d: &VirtualDrive, c: &Checkpoint, i: &Intent) -> Proposal {
    assert_eq!(i.size, 0);
    let archive = archive_id(&id(999), c.serial, &d.state.lock().unwrap().device, &i.id);
    let manifest = Manifest {
        version: 2,
        archive_id: archive.clone(),
        original_name: Path::new(&i.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into(),
        original_size: 0,
        shard_size: 1024,
        created_unix: 1,
        content_root_blake3: crate::manifest::content_root_v2(0, 1024, &None, &[]),
        coding: None,
        shards: vec![],
    };
    Proposal {
        version: 5,
        id: i.id.clone(),
        serial: c.serial,
        device: d.state.lock().unwrap().device.clone(),
        worker: "writer".into(),
        path: i.path.clone(),
        base: i.checkpoint_base.clone(),
        value: i.spool.as_ref().map(|_| ManagedContent {
            content: Content {
                hash: i.hash.clone(),
                size: 0,
                manifest,
            },
            objects: vec![format!("crypt:{archive}/manifest.json")],
        }),
    }
}
fn accept(d: &VirtualDrive, c: &Checkpoint, i: &Intent) -> Checkpoint {
    let (next, _) = c.apply_batch(&[proposal(d, c, i)], 1).unwrap();
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    next
}
#[test]
fn sequential_acknowledged_edits_bound_history_and_spool() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, mut c) = setup(tmp.path());
    for _ in 0..30 {
        let i = write(&d, "file", b"");
        assert_eq!(i.checkpoint_base, c.files.get("file").map(|v| v.id.clone()));
        c = accept(&d, &c, &i);
        d.cleanup_checkpoint_spool().unwrap();
        assert_eq!(c.files.len(), 1);
        assert!(c.history.values().map(Vec::len).sum::<usize>() <= 1);
        assert!(d.state.lock().unwrap().pending.is_empty());
        assert!(!d.spool_path(&i).exists());
    }
}
#[test]
fn publication_is_not_acknowledgement_and_live_reader_delays_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let memory = Memory::default();
    transport::publish_proposal(&memory, &proposal(&d, &c, &i)).unwrap();
    d.install_checkpoint(&pointer(&c), &c).unwrap();
    d.cleanup_checkpoint_spool().unwrap();
    assert_eq!(d.state.lock().unwrap().pending.len(), 1);
    assert!(d.spool_path(&i).exists());
    let reader = d.view().unwrap()["file"].clone();
    let _c = accept(&d, &c, &i);
    d.cleanup_checkpoint_spool().unwrap();
    assert!(d.spool_path(&i).exists());
    drop(reader);
    d.cleanup_checkpoint_spool().unwrap();
    assert!(!d.spool_path(&i).exists());
}
#[test]
fn remote_deletion_clears_pin_and_expired_handle_cannot_recreate_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let c = accept(&d, &c, &i);
    let old = d.view().unwrap()["file"].clone();
    d.pin_read("file", &old).unwrap();
    let mut deletion = proposal(&d, &c, &i);
    deletion.id = id(777);
    deletion.base = Some(i.id);
    deletion.value = None;
    let (next, removed) = c.apply_batch(&[deletion], 1).unwrap();
    assert!(!removed.is_empty());
    assert!(next.history.is_empty());
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    assert!(!d.view().unwrap().contains_key("file"));
    assert!(d.begin_observed("file", Some(&old)).is_err());
}
#[test]
fn stale_unsynced_chain_is_exported_without_resurrection() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let a = write(&d, "file", b"offline A");
    let b = write(&d, "file", b"offline B");
    assert_eq!(b.checkpoint_base, Some(a.id.clone()));
    // Any new authoritative epoch expires these unobserved create requests.
    let (next, _) = c.compact(1).unwrap();
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    assert!(d.state.lock().unwrap().pending.is_empty());
    assert!(d.view().unwrap().is_empty());
    for i in [&a, &b] {
        assert_eq!(
            fs::read(
                tmp.path()
                    .join("recovered-writes")
                    .join(format!("{}.stale.bin", i.id))
            )
            .unwrap(),
            fs::read(d.spool_path(i)).unwrap()
        );
    }
    d.cleanup_checkpoint_spool().unwrap();
    assert!(!d.spool_path(&a).exists());
}
#[test]
fn move_waits_for_destination_acknowledgement_before_source_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "old", b"");
    let c = accept(&d, &c, &i);
    d.rename_file("old", "new").unwrap();
    let pending = d.state.lock().unwrap().pending.clone();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[1].depends_on, Some(pending[0].id.clone()));
    assert!(matches!(d.view().unwrap()["new"], Revision::Local { .. }));
    let c = accept(&d, &c, &pending[0]);
    let deletion = d.state.lock().unwrap().pending[0].clone();
    assert!(deletion.depends_on.is_none());
    let c = accept(&d, &c, &deletion);
    assert!(c.files.contains_key("new"));
    assert!(!c.files.contains_key("old"));
}
#[test]
fn rollback_and_owner_change_preserve_local_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let next = accept(&d, &c, &i);
    assert!(d.install_checkpoint(&pointer(&c), &c).is_err());
    let mut changed = next.clone();
    changed.owner = id(998);
    assert!(d.install_checkpoint(&pointer(&changed), &changed).is_err());
    assert_eq!(
        d.state.lock().unwrap().checkpoint.as_ref().unwrap().serial,
        next.serial
    );
}
#[derive(Default)]
struct Memory {
    files: RefCell<BTreeMap<String, Vec<u8>>>,
    objects: RefCell<BTreeSet<String>>,
}
impl CheckpointIo for Memory {
    fn read(&self, p: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.files.borrow().get(p).cloned())
    }
    fn put(&self, p: &str, b: &[u8]) -> Result<()> {
        self.files.borrow_mut().insert(p.into(), b.into());
        Ok(())
    }
    fn list(&self, p: &str) -> Result<Vec<String>> {
        Ok(self
            .files
            .borrow()
            .keys()
            .filter_map(|k| k.strip_prefix(&format!("{p}/")).map(str::to_owned))
            .collect())
    }
    fn remove(&self, p: &str) -> Result<()> {
        self.files.borrow_mut().remove(p);
        Ok(())
    }
    fn remove_object(&self, p: &str) -> Result<()> {
        self.objects.borrow_mut().remove(p);
        Ok(())
    }
    fn verify(&self, v: &ManagedContent) -> Result<()> {
        v.validate()
    }
}
fn ledger(io: &Memory, n: u64, finished: bool) -> (String, UploadLedger) {
    let archive = archive_id(&id(999), 0, &id(998), &id(n));
    let l = UploadLedger {
        version: 5,
        objects: vec![format!("crypt:{archive}/manifest.json")],
        archive,
        attempt: id(n),
        serial: 0,
        finished,
    };
    let key = ledger_key(&l.archive, &l.objects, &l.attempt).unwrap();
    io.put(&key, &serde_json::to_vec(&l).unwrap()).unwrap();
    io.objects.borrow_mut().extend(l.objects.clone());
    (key, l)
}
#[test]
fn unfinished_late_upload_is_reswept_without_starving_completed_ledgers() {
    let io = Memory::default();
    let mut c = Checkpoint::empty(&id(997));
    c.serial = 1;
    for n in 0..300 {
        ledger(&io, n, false);
    }
    let (key, _) = ledger(&io, 1000, true);
    sweep_uploads(&io, &id(999), &c).unwrap();
    assert!(io.read(&key).unwrap().is_none());
    assert!(io.objects.borrow().is_empty());
    assert_eq!(io.files.borrow().len(), 300);
    let (_, late) = ledger(&io, 1, false);
    sweep_uploads(&io, &id(999), &c).unwrap();
    assert!(!io.objects.borrow().contains(&late.objects[0]));
}
#[test]
fn protected_archive_keeps_alternate_layout_ledger_until_retirement() {
    let io = Memory::default();
    let (key, l) = ledger(&io, 1, true);
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let mut p = proposal(&d, &c, &i);
    p.value.as_mut().unwrap().content.manifest.archive_id = l.archive.clone();
    p.value.as_mut().unwrap().objects = l.objects.clone();
    let mut cp = Checkpoint::empty(&id(997));
    cp.serial = 1;
    cp.files.insert(
        "file".into(),
        Version {
            id: id(1),
            value: p.value.unwrap(),
        },
    );
    sweep_uploads(&io, &id(999), &cp).unwrap();
    assert!(io.read(&key).unwrap().is_some());
    cp.files.clear();
    sweep_uploads(&io, &id(999), &cp).unwrap();
    assert!(io.read(&key).unwrap().is_none());
    assert!(io.objects.borrow().is_empty());
}

#[test]
fn coordinator_checks_high_water_before_any_remote_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let _next = accept(&d, &c, &i);
    let io = Memory::default();
    assert!(d.validate_checkpoint_high_water(&io).is_err());
    io.put("current.json", &serde_json::to_vec(&pointer(&c)).unwrap())
        .unwrap();
    io.put(
        &format!("checkpoints/{}.json", pointer(&c).hash),
        &serde_json::to_vec(&c).unwrap(),
    )
    .unwrap();
    let before = io.files.borrow().clone();
    assert!(d.validate_checkpoint_high_water(&io).is_err());
    assert_eq!(*io.files.borrow(), before);
}
#[test]
fn previous_native_cache_is_isolated_without_replay_or_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, _) = setup(tmp.path());
    let cache = tmp.path().join("vfs-cache");
    fs::create_dir_all(cache.join("vfs/old")).unwrap();
    fs::write(cache.join("vfs/old/file"), b"unflushed native data").unwrap();
    d.isolate_previous_native_cache().unwrap();
    assert!(!cache.exists());
    let isolated = fs::read_dir(tmp.path().join("recovered-native-cache"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read(isolated.join("vfs/old/file")).unwrap(),
        b"unflushed native data"
    );
}
#[test]
fn remote_refresh_does_not_grant_a_stale_editor_overwrite_permission() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let c = accept(&d, &c, &i);
    let old = d.view().unwrap()["file"].clone();
    d.pin_read("file", &old).unwrap();
    let mut remote = proposal(&d, &c, &i);
    remote.id = id(444);
    remote.base = Some(i.id.clone());
    let value = remote.value.as_mut().unwrap();
    value.content.manifest.archive_id = archive_id(&id(999), c.serial, &remote.device, &remote.id);
    value.objects = vec![format!(
        "crypt:{}/manifest.json",
        value.content.manifest.archive_id
    )];
    let (next, _) = c.apply_batch(&[remote], 1).unwrap();
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    let stale = write(&d, "file", b"");
    assert_eq!(stale.checkpoint_base, Some(i.id));
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    assert!(d.state.lock().unwrap().pending.is_empty());
    assert_eq!(next.files["file"].id, id(444));
    assert_eq!(next.files.len(), 1);
    assert!(tmp
        .path()
        .join("recovered-writes")
        .join(format!("{}.stale.bin", stale.id))
        .exists());
}

#[test]
fn own_acknowledgement_does_not_expire_other_queued_new_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let a = write(&d, "a", b"");
    let b = write(&d, "b", b"");
    let c = accept(&d, &c, &a);
    assert_eq!(d.state.lock().unwrap().pending[0].id, b.id);
    let pending = d.state.lock().unwrap().pending[0].clone();
    let c = accept(&d, &c, &pending);
    assert_eq!(c.files.len(), 2);
    assert!(!tmp.path().join("recovered-writes").exists());
}
#[test]
fn expired_read_cannot_repin_deleted_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let c = accept(&d, &c, &i);
    let old = d.view().unwrap()["file"].clone();
    let mut next = c.clone();
    next.serial += 1;
    next.files.clear();
    next.history.clear();
    next.receipts.clear();
    d.install_checkpoint(&pointer(&next), &next).unwrap();
    assert!(d.pin_read("file", &old).is_err());
    assert!(d.view().unwrap().is_empty());
}

#[test]
fn directory_move_drains_each_copy_before_its_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let a = write(&d, "old/a", b"");
    let b = write(&d, "old/b", b"");
    let c = accept(&d, &c, &a);
    let mut c = accept(&d, &c, &b);
    d.rename_directory("old", "new").unwrap();
    for _ in 0..4 {
        let i = d.state.lock().unwrap().pending[0].clone();
        assert!(i.depends_on.is_none());
        c = accept(&d, &c, &i);
    }
    assert!(d.state.lock().unwrap().pending.is_empty());
    assert_eq!(
        c.files.keys().cloned().collect::<Vec<_>>(),
        vec!["new/a", "new/b"]
    );
    assert!(!tmp.path().join("recovered-writes").exists());
}
#[test]
fn late_local_read_does_not_pin_retired_spool_back_into_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let i = write(&d, "file", b"");
    let reader = d.view().unwrap()["file"].clone();
    let _c = accept(&d, &c, &i);
    assert!(d.pin_read("file", &reader).is_err());
    assert!(matches!(d.view().unwrap()["file"], Revision::Cloud { .. }));
}

#[test]
fn acknowledged_local_delete_allows_deliberate_recreation() {
    let tmp = tempfile::tempdir().unwrap();
    let (d, c) = setup(tmp.path());
    let first = write(&d, "file", b"");
    let c = accept(&d, &c, &first);
    d.delete("file").unwrap();
    let deletion = d.state.lock().unwrap().pending[0].clone();
    let c = accept(&d, &c, &deletion);
    let recreated = write(&d, "file", b"");
    assert!(recreated.checkpoint_base.is_none());
    d.install_checkpoint(&pointer(&c), &c).unwrap();
    let c = accept(&d, &c, &recreated);
    assert_eq!(c.files["file"].id, recreated.id);
}
