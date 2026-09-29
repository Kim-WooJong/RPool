use super::super::peer_snapshot_transport::{copy_private_key, resume_gc_with, PayloadIo};
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Backend {
    records: BTreeMap<(String, String), Vec<u8>>,
    payloads: BTreeMap<String, Vec<u8>>,
    uploads: usize,
    copies: usize,
    deleted: Vec<String>,
    fail_publish_once: bool,
    fail_collect: bool,
    publications: usize,
    collections: usize,
}
#[derive(Clone, Default)]
struct FakeIo(Rc<RefCell<Backend>>);
impl PayloadIo for FakeIo {
    fn verify_if_present(&self, s: &Shard) -> Result<bool> {
        let state = self.0.borrow();
        let Some(bytes) = state.payloads.get(&s.object) else {
            return Ok(false);
        };
        if bytes.len() as u64 != s.size || blake3::hash(bytes).to_hex().as_str() != s.blake3 {
            bail!("bad synthetic payload")
        }
        Ok(true)
    }
    fn copy(&self, source: &Shard, target: &Shard) -> Result<()> {
        if !self.verify_if_present(source)? {
            bail!("missing synthetic source")
        }
        let bytes = self.0.borrow().payloads[&source.object].clone();
        let mut state = self.0.borrow_mut();
        state.copies += 1;
        state.payloads.insert(target.object.clone(), bytes);
        Ok(())
    }
    fn remove(&self, key: &str) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.deleted.push(key.into());
        state.payloads.remove(key);
        Ok(())
    }
}
impl Io for FakeIo {
    fn collect(&self, kind: &str, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        if self.0.borrow().fail_collect {
            bail!("injected metadata outage");
        }
        Ok(self
            .0
            .borrow()
            .records
            .iter()
            .filter(|((k, id), _)| k == kind && !known.contains(id))
            .map(|((_, id), b)| (id.clone(), b.clone()))
            .collect())
    }
    fn publish(&self, kind: &str, id: &str, bytes: &[u8]) -> Result<()> {
        if blake3::hash(bytes).to_hex().as_str() != id {
            bail!("bad record hash")
        }
        let mut state = self.0.borrow_mut();
        let key = (kind.into(), id.into());
        if state.records.get(&key).is_some_and(|b| b != bytes) {
            bail!("immutable overwrite")
        }
        state.publications += 1;
        state.records.insert(key, bytes.to_vec());
        if state.fail_publish_once {
            state.fail_publish_once = false;
            bail!("injected acknowledged publication failure")
        }
        Ok(())
    }
    fn copy(&self, owner: &str, revision: &str, m: &Manifest) -> Result<Manifest> {
        copy_private_key(self, owner, revision, m)
    }
    fn upload(&self, source: &Path, archive: &str) -> Result<Manifest> {
        let bytes = fs::read(source)?;
        let size = bytes.len() as u64;
        let shards = vec![Shard {
            index: 0,
            offset: 0,
            size,
            remote: "crypt:".into(),
            object: format!("crypt:{archive}/data/0"),
            blake3: blake3::hash(&bytes).to_hex().to_string(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        }];
        let manifest = Manifest {
            version: 2,
            archive_id: archive.into(),
            original_name: source.file_name().unwrap().to_string_lossy().into_owned(),
            original_size: size,
            shard_size: size.max(1),
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(
                size,
                size.max(1),
                &None,
                &shards,
            ),
            coding: None,
            shards,
        };
        crate::manifest::validate_manifest(&manifest)?;
        let mut state = self.0.borrow_mut();
        state.uploads += 1;
        let key = manifest.shards[0].object.clone();
        if state.payloads.get(&key).is_some_and(|old| old != &bytes) {
            bail!("payload overwrite")
        }
        state.payloads.insert(key, bytes);
        Ok(manifest)
    }
    fn verify(&self, m: &Manifest) -> Result<()> {
        for shard in &m.shards {
            if !self.verify_if_present(shard)? {
                bail!("missing payload")
            }
        }
        Ok(())
    }
    fn capture(&self, manifest: &Manifest, output: &Path) -> Result<()> {
        self.verify(manifest)?;
        let mut bytes = Vec::new();
        for shard in crate::manifest::data_shards(manifest) {
            bytes.extend_from_slice(&self.0.borrow().payloads[&shard.object]);
        }
        fs::write(output, bytes)?;
        Ok(())
    }
    fn gc(&self, path: &Path, proof: &mut dyn FnMut(&str, &str) -> Result<()>) -> Result<()> {
        self.0.borrow_mut().collections += 1;
        resume_gc_with(self, path, proof)
    }
}
fn stage(d: &VirtualDrive, io: &FakeIo, path: &str, bytes: &[u8]) {
    // A new editor/read session observes the current version, not an old DAV pin.
    d.peer_read_pins.lock().unwrap().clear();
    let visible = d.view().unwrap().remove(path);
    if let Some(ref revision) = visible {
        d.pin_read(path, revision).unwrap();
    }
    let intent = d.begin_snapshot_with(path, visible.as_ref(), io).unwrap();
    fs::write(d.spool_path(&intent), bytes).unwrap();
    d.seal(intent).unwrap();
}
fn visible_bytes(d: &VirtualDrive, io: &FakeIo) -> BTreeMap<String, Vec<u8>> {
    d.view()
        .unwrap()
        .into_iter()
        .map(|(path, r)| {
            let super::super::virtual_drive::Revision::Cloud { content, .. } = r else {
                panic!("uncommitted view")
            };
            Io::verify(io, &content.manifest).unwrap();
            let mut bytes = Vec::new();
            for shard in crate::manifest::data_shards(&content.manifest) {
                bytes.extend_from_slice(&io.0.borrow().payloads[&shard.object]);
            }
            (path, bytes)
        })
        .collect()
}

#[test]
fn injected_runtime_history_limits_delete_old_payloads_and_keep_current_bytes() {
    for limit in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let mut d = drive(root.path());
        d.pool_history_limit = limit;
        let io = FakeIo::default();
        for text in [b"version0", b"version1", b"version2"] {
            stage(&d, &io, "file", text);
            d.sync_snapshots_with(&io).unwrap();
            assert_eq!(visible_bytes(&d, &io)["file"], text);
        }
        assert!(!io.0.borrow().deleted.is_empty());
        let live: BTreeSet<_> = io.0.borrow().payloads.values().cloned().collect();
        assert!(!live.contains(b"version0".as_slice()));
        assert_eq!(live.contains(b"version1".as_slice()), limit == 1);
        assert!(live.contains(b"version2".as_slice()));
        let before = (io.0.borrow().uploads, io.0.borrow().copies);
        d.rename_file("file", "renamed").unwrap();
        d.sync_snapshots_with(&io).unwrap();
        assert_eq!((io.0.borrow().uploads, io.0.borrow().copies), before);
        assert_eq!(visible_bytes(&d, &io)["renamed"], b"version2");
    }
}

#[test]
fn injected_runtime_disjoint_pcs_and_publication_retry() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = drive(root_a.path());
    let b = drive(root_b.path());
    let io = FakeIo::default();
    stage(&a, &io, "alpha", b"alpha");
    stage(&b, &io, "beta", b"beta");
    io.0.borrow_mut().fail_publish_once = true;
    assert!(a.sync_snapshots_with(&io).is_err());
    assert_eq!(a.state.lock().unwrap().pending.len(), 1);
    a.sync_snapshots_with(&io).unwrap();
    assert_eq!(io.0.borrow().uploads, 1);
    b.sync_snapshots_with(&io).unwrap();
    a.sync_snapshots_with(&io).unwrap();
    assert_eq!(
        visible_bytes(&a, &io),
        BTreeMap::from([
            ("alpha".into(), b"alpha".to_vec()),
            ("beta".into(), b"beta".to_vec())
        ])
    );
    assert_eq!(visible_bytes(&a, &io), visible_bytes(&b, &io));
}

#[test]
fn injected_runtime_same_base_siblings_preserve_retained_original_and_both_edits() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let mut a = drive(root_a.path());
    let mut b = drive(root_b.path());
    a.pool_history_limit = 1;
    b.pool_history_limit = 1;
    b.state.lock().unwrap().worker = "second".into();
    let io = FakeIo::default();
    stage(&a, &io, "file", b"original");
    a.sync_snapshots_with(&io).unwrap();
    b.sync_snapshots_with(&io).unwrap();
    stage(&a, &io, "file", b"edit-a");
    stage(&b, &io, "file", b"edit-b");
    a.sync_snapshots_with(&io).unwrap();
    b.sync_snapshots_with(&io).unwrap();
    a.sync_snapshots_with(&io).unwrap();
    let files = visible_bytes(&a, &io);
    assert_eq!(files.len(), 3);
    assert_eq!(files["file"], b"original");
    assert_eq!(
        files.values().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([b"original".to_vec(), b"edit-a".to_vec(), b"edit-b".to_vec()])
    );
    assert_eq!(files, visible_bytes(&b, &io));
    let candidate = files.keys().find(|path| path.as_str() != "file").unwrap();
    let before = serde_json::to_vec(&a.snapshot_state().unwrap().names).unwrap();
    assert!(a.rename_file(candidate, "selected-branch").is_err());
    assert_eq!(
        serde_json::to_vec(&a.snapshot_state().unwrap().names).unwrap(),
        before
    );
    assert_eq!(visible_bytes(&a, &io), files);
}

#[test]
fn injected_runtime_delete_then_recreate_keeps_identity_and_no_conflict() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "file", b"before deletion");
    d.sync_snapshots_with(&io).unwrap();
    let original_file = identity(&d, "file").0;
    d.delete("file").unwrap();
    d.sync_snapshots_with(&io).unwrap();
    assert!(d.view().unwrap().is_empty());
    assert!(io.0.borrow().payloads.is_empty());
    stage(&d, &io, "file", b"deliberate recreation");
    d.sync_snapshots_with(&io).unwrap();
    assert_eq!(
        visible_bytes(&d, &io),
        BTreeMap::from([("file".into(), b"deliberate recreation".to_vec())])
    );
    assert_eq!(identity(&d, "file").0, original_file);
    assert!(d.snapshot_conflicts().unwrap().is_empty());
}

#[test]
fn injected_runtime_history_zero_offline_sibling_uses_captured_original() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = drive(root_a.path());
    let b = drive(root_b.path());
    b.state.lock().unwrap().worker = "second".into();
    let io = FakeIo::default();
    stage(&a, &io, "file", b"original");
    a.sync_snapshots_with(&io).unwrap();
    b.sync_snapshots_with(&io).unwrap();
    stage(&a, &io, "file", b"edit-a");
    stage(&b, &io, "file", b"edit-b");
    a.sync_snapshots_with(&io).unwrap();
    assert!(!io
        .0
        .borrow()
        .payloads
        .values()
        .any(|bytes| bytes == b"original"));
    b.sync_snapshots_with(&io).unwrap();
    a.sync_snapshots_with(&io).unwrap();
    let files = visible_bytes(&a, &io);
    assert_eq!(files.len(), 3);
    assert_eq!(files["file"], b"original");
    assert_eq!(
        files.values().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([b"original".to_vec(), b"edit-a".to_vec(), b"edit-b".to_vec()])
    );
    assert_eq!(files, visible_bytes(&b, &io));
}

#[test]
fn injected_runtime_saved_ready_rejects_changed_history_policy() {
    let root = tempfile::tempdir().unwrap();
    let mut d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "file", b"first");
    io.0.borrow_mut().fail_publish_once = true;
    assert!(d.sync_snapshots_with(&io).is_err());
    let intent = d.state.lock().unwrap().pending[0].clone();
    let path = d
        .root
        .join("spool")
        .join(&intent.id)
        .join("snapshot-plan.json");
    let mut plan: Plan = load(&path).unwrap();
    assert!(plan.ready.is_some());
    let state = d.snapshot_state().unwrap();
    let before = (
        io.0.borrow().uploads,
        io.0.borrow().copies,
        io.0.borrow().records.len(),
    );
    d.pool_history_limit = 1;
    assert!(d
        .finish_plan(&state, &io, &path, &mut plan, Some(&d.spool_path(&intent)))
        .is_err());
    assert_eq!(
        (
            io.0.borrow().uploads,
            io.0.borrow().copies,
            io.0.borrow().records.len()
        ),
        before
    );
}

fn token(n: u64) -> String {
    format!("{n:064x}")
}
fn drive(root: &Path) -> VirtualDrive {
    let mut drive = super::super::virtual_drive::fixture(root);
    drive.peer_retention = true;
    drive.pool_sync_roots = vec!["crypt:synthetic-v7".into()];
    drive.policy.remotes = vec!["crypt:".into()];
    drive.rclone = root
        .join("nonexistent-rclone")
        .to_string_lossy()
        .into_owned();
    drive.state.lock().unwrap().version = 7;
    drive
}
fn add_file(drive: &VirtualDrive, state: &mut State, number: u64, path: &str) -> (String, String) {
    let file = token(number);
    let owner = token(number + 100);
    let bytes_hash = blake3::hash(b"same bytes").to_hex().to_string();
    let revision = Revision {
        parents: BTreeSet::new(),
        content_hash: Some(bytes_hash.clone()),
        size: 10,
        worker: "worker".into(),
        device: "device".into(),
    };
    let revision_id = revision.id().unwrap();
    let archive = model::owner_archive(&owner);
    let shards = vec![Shard {
        index: 0,
        offset: 0,
        size: 10,
        remote: "crypt:".into(),
        object: format!("crypt:{archive}/data/0"),
        blake3: bytes_hash,
        kind: ShardKind::Data,
        group: 0,
        slot: 0,
    }];
    let manifest = Manifest {
        version: 2,
        archive_id: archive,
        original_name: path.into(),
        original_size: 10,
        shard_size: 10,
        created_unix: 0,
        content_root_blake3: crate::manifest::content_root_v2(10, 10, &None, &shards),
        coding: None,
        shards,
    };
    let snapshot = model::build(
        file.clone(),
        owner,
        drive.snapshot_policy().unwrap(),
        BTreeSet::new(),
        &state.snapshots,
        BTreeMap::from([(revision_id.clone(), revision)]),
        BTreeMap::from([(revision_id.clone(), manifest)]),
    )
    .unwrap();
    state.snapshots.insert(snapshot.id().unwrap(), snapshot);
    let name = NameOp {
        nonce: token(number + 200),
        entries: BTreeMap::from([(file.clone(), path.into())]),
        parents: BTreeMap::from([(file.clone(), BTreeSet::new())]),
    };
    state.names.insert(hash(&name).unwrap(), name);
    (file, revision_id)
}
fn materialize(drive: &VirtualDrive, state: &mut State) {
    validate_names(state).unwrap();
    let analysis = model::analyze(&state.snapshots, &drive.snapshot_policy().unwrap()).unwrap();
    drive.save_snapshot_state(state).unwrap();
    drive.materialize_snapshots(state, &analysis).unwrap();
}
fn identity(drive: &VirtualDrive, path: &str) -> (String, String, Vec<u8>) {
    let namespace = drive.state.lock().unwrap();
    let event = &namespace.snapshot_view[path];
    let state = drive.snapshot_state().unwrap();
    let target = &state.mappings[event];
    (
        target.file.clone(),
        target.revision.clone(),
        serde_json::to_vec(&namespace.events[event].content.as_ref().unwrap().manifest).unwrap(),
    )
}

#[test]
fn committed_file_moves_are_metadata_only_and_keep_edit_baseline() {
    let root = tempfile::tempdir().unwrap();
    let drive = drive(root.path());
    let mut state = State::default();
    let (file, revision) = add_file(&drive, &mut state, 1, "before");
    materialize(&drive, &mut state);
    let before = identity(&drive, "before");
    let snapshots = serde_json::to_vec(&state.snapshots).unwrap();
    drive.rename_file("before", "middle").unwrap();
    drive.rename_file("middle", "after").unwrap();
    assert_eq!(identity(&drive, "after"), before);
    assert_eq!(
        drive.view().unwrap().keys().cloned().collect::<Vec<_>>(),
        vec!["after"]
    );
    let state = drive.snapshot_state().unwrap();
    assert_eq!(serde_json::to_vec(&state.snapshots).unwrap(), snapshots);
    assert!(drive.state.lock().unwrap().pending.is_empty());
    let visible = drive.view().unwrap().remove("after").unwrap();
    drive.pin_read("after", &visible).unwrap();
    let intent = drive.begin_intent("after", Some(&visible)).unwrap();
    let analysis = model::analyze(&state.snapshots, &drive.snapshot_policy().unwrap()).unwrap();
    let plan = drive.make_plan(&state, &analysis, &intent).unwrap();
    assert_eq!(plan.file, file);
    assert_eq!(
        plan.revisions.values().next().unwrap().parents,
        BTreeSet::from([revision])
    );
    assert!(plan.name.is_none());
}

#[test]
fn directory_move_is_one_atomic_name_envelope_with_independent_equal_byte_files() {
    let root = tempfile::tempdir().unwrap();
    let drive = drive(root.path());
    let mut state = State::default();
    let a = add_file(&drive, &mut state, 1, "folder/a");
    let b = add_file(&drive, &mut state, 2, "folder/b");
    assert_ne!(a.0, b.0);
    assert_eq!(a.1, b.1); // Content equality must not merge logical file identities.
    materialize(&drive, &mut state);
    let before_a = identity(&drive, "folder/a");
    let before_b = identity(&drive, "folder/b");
    let count = state.names.len();
    drive.rename_directory("folder", "moved").unwrap();
    let moved = drive.snapshot_state().unwrap();
    let added: Vec<_> = moved
        .names
        .iter()
        .filter(|(id, _)| !state.names.contains_key(*id))
        .collect();
    assert_eq!(moved.names.len(), count + 1);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].1.entries.len(), 2);
    assert_eq!(identity(&drive, "moved/a"), before_a);
    assert_eq!(identity(&drive, "moved/b"), before_b);
    assert_eq!(
        drive.view().unwrap().keys().cloned().collect::<Vec<_>>(),
        vec!["moved/a", "moved/b"]
    );
    drive.rename_directory("moved", "final").unwrap();
    assert_eq!(identity(&drive, "final/a"), before_a);
    assert_eq!(identity(&drive, "final/b"), before_b);
}

#[test]
fn concurrent_renames_converge_without_discarding_either_name() {
    let root = tempfile::tempdir().unwrap();
    let drive = drive(root.path());
    let mut base = State::default();
    let (file, _) = add_file(&drive, &mut base, 1, "start");
    let parents = name_heads(&base, &file);
    let operations: Vec<_> = [(300, "left"), (301, "right")]
        .into_iter()
        .map(|(n, path)| {
            let op = NameOp {
                nonce: token(n),
                entries: BTreeMap::from([(file.clone(), path.into())]),
                parents: BTreeMap::from([(file.clone(), parents.clone())]),
            };
            (hash(&op).unwrap(), op)
        })
        .collect();
    let mut left = base.clone();
    for (id, op) in &operations {
        left.names.insert(id.clone(), op.clone());
    }
    materialize(&drive, &mut left);
    let expected = drive.state.lock().unwrap().snapshot_view.clone();
    assert_eq!(
        expected.keys().cloned().collect::<Vec<_>>(),
        vec!["left", "right"]
    );
    assert_eq!(identity(&drive, "left"), identity(&drive, "right"));
    let mut right = base;
    for (id, op) in operations.into_iter().rev() {
        right.names.insert(id, op);
    }
    materialize(&drive, &mut right);
    assert_eq!(drive.state.lock().unwrap().snapshot_view, expected);
    assert_eq!(name_heads(&right, &file).len(), 2);
}

#[test]
fn metadata_bootstrap_preserves_pending_spool_without_cloud_mutations() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = drive(root_a.path());
    let b = drive(root_b.path());
    let io = FakeIo::default();
    stage(&a, &io, "unsent", b"durable local bytes");
    stage(&b, &io, "remote", b"current cloud bytes");
    b.sync_snapshots_with(&io).unwrap();
    let pending = a.state.lock().unwrap().pending.clone();
    let pending_before = serde_json::to_vec(&pending).unwrap();
    let mutations = {
        let backend = io.0.borrow();
        (
            backend.uploads,
            backend.copies,
            backend.publications,
            backend.collections,
            backend.deleted.clone(),
        )
    };

    a.pull_snapshots_with(&io).unwrap();

    assert_eq!(
        serde_json::to_vec(&a.state.lock().unwrap().pending).unwrap(),
        pending_before
    );
    assert_eq!(
        fs::read(a.spool_path(&pending[0])).unwrap(),
        b"durable local bytes"
    );
    let view = a.view().unwrap();
    assert!(matches!(
        view["unsent"],
        super::super::virtual_drive::Revision::Local { .. }
    ));
    let super::super::virtual_drive::Revision::Cloud { content, .. } = &view["remote"] else {
        panic!("remote metadata was not materialized");
    };
    assert_eq!(
        content.hash,
        blake3::hash(b"current cloud bytes").to_hex().to_string()
    );
    let backend = io.0.borrow();
    assert_eq!(
        (
            backend.uploads,
            backend.copies,
            backend.publications,
            backend.collections,
            backend.deleted.clone()
        ),
        mutations
    );
    drop(backend);

    // Bootstrap left the intent usable by the ordinary durable sync path.
    a.sync_snapshots_with(&io).unwrap();
    assert!(a.state.lock().unwrap().pending.is_empty());
    assert_eq!(visible_bytes(&a, &io)["unsent"], b"durable local bytes");
}

#[test]
fn metadata_bootstrap_outage_or_invalid_record_keeps_local_state_and_spool() {
    for invalid_record in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let d = drive(root.path());
        let io = FakeIo::default();
        stage(&d, &io, "unsent", b"retain me");
        let pending = d.state.lock().unwrap().pending.clone();
        let namespace_before = serde_json::to_vec(&*d.state.lock().unwrap()).unwrap();
        if invalid_record {
            io.0.borrow_mut().records.insert(
                ("snapshots".into(), "invalid-identity".into()),
                b"{}".to_vec(),
            );
        } else {
            io.0.borrow_mut().fail_collect = true;
        }

        assert!(d.pull_snapshots_with(&io).is_err());

        assert_eq!(
            serde_json::to_vec(&*d.state.lock().unwrap()).unwrap(),
            namespace_before
        );
        assert_eq!(fs::read(d.spool_path(&pending[0])).unwrap(), b"retain me");
        let backend = io.0.borrow();
        assert_eq!(
            (
                backend.uploads,
                backend.copies,
                backend.publications,
                backend.collections
            ),
            (0, 0, 0, 0)
        );
        assert!(backend.deleted.is_empty());
    }
}
