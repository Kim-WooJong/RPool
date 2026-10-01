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
    /// Records only in metadata checkpoints (deleted from the record folders).
    checkpointed: BTreeMap<(String, String), Vec<u8>>,
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
    fn checkpointed(
        &self,
        known: &dyn Fn(&str, &str) -> bool,
        _bootstrap: bool,
    ) -> Result<super::super::metadata_pool::Checkpointed> {
        let mut result = super::super::metadata_pool::Checkpointed::default();
        for ((kind, id), bytes) in &self.0.borrow().checkpointed {
            result
                .covered
                .entry(kind.clone())
                .or_default()
                .insert(id.clone());
            if !known(kind, id) {
                result.records.push((
                    kind.clone(),
                    id.clone(),
                    String::from_utf8(bytes.clone()).unwrap(),
                ));
            }
        }
        Ok(result)
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
fn records_only_in_checkpoints_bootstrap_a_new_pc_and_are_not_republished() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = drive(root_a.path());
    let b = drive(root_b.path());
    let io = FakeIo::default();
    stage(&a, &io, "alpha", b"alpha");
    a.sync_snapshots_with(&io).unwrap();
    // Compaction moved every record into a checkpoint and deleted the objects.
    {
        let mut state = io.0.borrow_mut();
        let records = std::mem::take(&mut state.records);
        state.checkpointed = records;
    }
    let publications = io.0.borrow().publications;
    b.sync_snapshots_with(&io).unwrap();
    assert_eq!(
        visible_bytes(&b, &io),
        BTreeMap::from([("alpha".into(), b"alpha".to_vec())])
    );
    // Durable in the checkpoint: b does not resurrect them as objects.
    assert_eq!(io.0.borrow().publications, publications);
    assert!(io.0.borrow().records.is_empty());
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

#[test]
fn transition_semantic_receipts_accept_materialized_events_not_equal_content_revisions() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "first", b"identical");
    let first = d.state.lock().unwrap().pending[0].id.clone();
    d.sync_snapshots_with(&io).unwrap();
    stage(&d, &io, "other", b"identical");
    d.sync_snapshots_with(&io).unwrap();
    let allowed = d
        .transition_snapshot_events(&BTreeSet::from([first.clone()]))
        .unwrap();
    let namespace = d.state.lock().unwrap();
    let committed = &namespace.committed_intents[&first];
    let synthetic = &namespace.snapshot_view["first"];
    assert_ne!(committed, synthetic);
    assert!(allowed.contains(committed));
    assert!(allowed.contains(synthetic));
    assert!(!allowed.contains(&namespace.snapshot_view["other"]));
    drop(namespace);
    // Equal bytes in a later revision of the same file are not the original intent.
    stage(&d, &io, "first", b"identical");
    d.sync_snapshots_with(&io).unwrap();
    let allowed = d
        .transition_snapshot_events(&BTreeSet::from([first.clone()]))
        .unwrap();
    assert!(!allowed.contains(&d.state.lock().unwrap().snapshot_view["first"]));
    let mut state = d.snapshot_state().unwrap();
    state.published_names.clear();
    d.save_snapshot_state(&state).unwrap();
    assert!(d
        .transition_snapshot_events(&BTreeSet::from([first]))
        .is_err());
}

#[test]
fn transition_resumes_published_snapshot_before_namespace_commit() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "file", b"pending transition");
    let intent = d.state.lock().unwrap().pending[0].clone();
    let mut state = d.snapshot_state().unwrap();
    let analysis = model::analyze(&state.snapshots, &d.snapshot_policy().unwrap()).unwrap();
    let mut plan = d.make_plan(&state, &analysis, &intent).unwrap();
    let path = d
        .root
        .join("spool")
        .join(&intent.id)
        .join("snapshot-plan.json");
    save(&path, &plan).unwrap();
    let snapshot = d
        .finish_plan(&state, &io, &path, &mut plan, Some(&d.spool_path(&intent)))
        .unwrap();
    d.publish_plan(&mut state, &io, &plan, snapshot).unwrap();
    // Simulate crash here: publication completed, but namespace.commit did not run.
    // Collection can also precede durable local publication receipts.
    state.published_snapshots.clear();
    state.published_names.clear();
    materialize(&d, &mut state);
    assert!(!d
        .state
        .lock()
        .unwrap()
        .committed_intents
        .contains_key(&intent.id));
    let intents = BTreeSet::from([intent.id.clone()]);
    let allowed = d.transition_snapshot_events(&intents).unwrap();
    assert!(allowed.contains(&d.state.lock().unwrap().snapshot_view["file"]));
    let mut unrelated = plan.clone();
    unrelated.file = "f".repeat(64);
    save(&path, &unrelated).unwrap();
    assert!(d.transition_snapshot_events(&intents).is_err());
    save(&path, &plan).unwrap();
    d.sync_snapshots_with(&io).unwrap();
    assert!(d
        .state
        .lock()
        .unwrap()
        .committed_intents
        .contains_key(&intent.id));
    let allowed = d.transition_snapshot_events(&intents).unwrap();
    assert!(allowed.contains(&d.state.lock().unwrap().snapshot_view["file"]));
    assert!(d.state.lock().unwrap().pending.is_empty());
}

// ---- Native frontend (fs_core) entry points ----

use crate::mount::native_ancestry::{Ancestry, Busy, CrossDevice};

/// A native write: explicit ancestry, sealed like a native close.
fn native_write(d: &VirtualDrive, io: &FakeIo, path: &str, bytes: &[u8], ancestry: &Ancestry) {
    let visible = d.view().unwrap().remove(path);
    let intent = d
        .begin_snapshot_based_with(path, visible.as_ref(), ancestry, io)
        .unwrap();
    fs::write(d.spool_path(&intent), bytes).unwrap();
    d.seal(intent).unwrap();
}
fn event_at(d: &VirtualDrive, path: &str) -> String {
    d.view().unwrap()[path].id().to_string()
}
fn spool_dirs(d: &VirtualDrive) -> usize {
    fs::read_dir(d.root.join("spool")).unwrap().count()
}

#[test]
fn native_atomic_save_becomes_the_next_revision_of_the_target() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "doc", b"version one");
    d.sync_snapshots_with(&io).unwrap();
    let base = event_at(&d, "doc");
    let files_before = d
        .snapshot_state()
        .unwrap()
        .snapshots
        .values()
        .map(|s| s.file_id.clone())
        .collect::<BTreeSet<_>>();
    // Editor: write a temp file (two closes), then rename it over the target.
    native_write(&d, &io, "doc.tmp", b"partial", &Ancestry::Default);
    native_write(&d, &io, "doc.tmp", b"version two", &Ancestry::Default);
    let before = spool_dirs(&d);
    d.rename_native_with(
        "doc.tmp",
        "doc",
        false,
        Some(&Ancestry::Parents(vec![base])),
        &io,
    )
    .unwrap();
    assert_eq!(
        spool_dirs(&d),
        before - 1,
        "the superseded temp image is dropped"
    );
    assert!(!d.view().unwrap().contains_key("doc.tmp"));
    d.sync_snapshots_with(&io).unwrap();
    let visible = visible_bytes(&d, &io);
    assert_eq!(
        visible,
        BTreeMap::from([("doc".into(), b"version two".to_vec())])
    );
    let files_after = d
        .snapshot_state()
        .unwrap()
        .snapshots
        .values()
        .map(|s| s.file_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        files_after, files_before,
        "same file identity, new revision"
    );
}

#[test]
fn native_atomic_save_after_a_peer_edit_is_a_preserved_conflict() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let mut a = drive(root_a.path());
    let mut b = drive(root_b.path());
    a.pool_history_limit = 1;
    b.pool_history_limit = 1;
    b.state.lock().unwrap().worker = "second".into();
    let io = FakeIo::default();
    stage(&a, &io, "doc", b"original");
    a.sync_snapshots_with(&io).unwrap();
    b.sync_snapshots_with(&io).unwrap();
    let base = event_at(&a, "doc");
    stage(&b, &io, "doc", b"edit-b");
    b.sync_snapshots_with(&io).unwrap();
    a.sync_snapshots_with(&io).unwrap();
    // A's editor read the original before B's edit arrived.
    native_write(&a, &io, "doc.tmp", b"edit-a", &Ancestry::Default);
    a.rename_native_with(
        "doc.tmp",
        "doc",
        false,
        Some(&Ancestry::Parents(vec![base])),
        &io,
    )
    .unwrap();
    a.sync_snapshots_with(&io).unwrap();
    b.sync_snapshots_with(&io).unwrap();
    a.sync_snapshots_with(&io).unwrap();
    let files = visible_bytes(&a, &io);
    assert_eq!(
        files.values().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([b"original".to_vec(), b"edit-a".to_vec(), b"edit-b".to_vec()]),
        "{:?}",
        files.keys()
    );
    assert_eq!(files, visible_bytes(&b, &io));
}

#[test]
fn native_moves_of_fresh_and_edited_files_work_while_pending() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    // A fresh file moved twice before its first upload.
    native_write(&d, &io, "new", b"fresh", &Ancestry::Default);
    d.rename_native_with("new", "dir/newer", false, None, &io)
        .unwrap();
    d.rename_native_with("dir/newer", "final", false, None, &io)
        .unwrap();
    // A synced file with a pending edit, moved.
    stage(&d, &io, "kept", b"one");
    d.sync_snapshots_with(&io).unwrap();
    let kept = event_at(&d, "kept");
    native_write(&d, &io, "kept", b"two", &Ancestry::Parents(vec![kept]));
    d.rename_native_with("kept", "moved", false, None, &io)
        .unwrap();
    assert!(!d.view().unwrap().contains_key("kept"));
    d.sync_snapshots_with(&io).unwrap();
    assert_eq!(
        visible_bytes(&d, &io),
        BTreeMap::from([
            ("final".into(), b"fresh".to_vec()),
            ("moved".into(), b"two".to_vec())
        ])
    );
}

#[test]
fn native_directory_move_carries_pending_and_synced_children() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "src/synced", b"s");
    d.sync_snapshots_with(&io).unwrap();
    native_write(&d, &io, "src/fresh", b"f", &Ancestry::Default);
    d.rename_native_with("src", "dst", true, None, &io).unwrap();
    d.sync_snapshots_with(&io).unwrap();
    assert_eq!(
        visible_bytes(&d, &io),
        BTreeMap::from([
            ("dst/fresh".into(), b"f".to_vec()),
            ("dst/synced".into(), b"s".to_vec())
        ])
    );
}

#[test]
fn native_renames_refuse_what_v7_cannot_express_without_changing_state() {
    let root = tempfile::tempdir().unwrap();
    let d = drive(root.path());
    let io = FakeIo::default();
    stage(&d, &io, "a", b"a");
    stage(&d, &io, "b", b"b");
    d.sync_snapshots_with(&io).unwrap();
    // A synced file renamed over another existing file.
    let error = d
        .rename_native_with("a", "b", false, None, &io)
        .unwrap_err();
    assert!(error.chain().any(|c| c.is::<CrossDevice>()), "{error:#}");
    // A chain whose upload already started.
    native_write(&d, &io, "c", b"c", &Ancestry::Default);
    let id = d.state.lock().unwrap().pending.last().unwrap().id.clone();
    fs::write(
        d.root.join("spool").join(&id).join("snapshot-plan.json"),
        b"{}",
    )
    .unwrap();
    let names = serde_json::to_vec(&d.snapshot_state().unwrap().names).unwrap();
    let error = d
        .rename_native_with("c", "d", false, None, &io)
        .unwrap_err();
    assert!(error.chain().any(|c| c.is::<Busy>()), "{error:#}");
    assert_eq!(
        serde_json::to_vec(&d.snapshot_state().unwrap().names).unwrap(),
        names
    );
    assert!(d.view().unwrap().contains_key("c"));
}

#[test]
fn native_delete_captures_the_original_it_descends_from() {
    let root = tempfile::tempdir().unwrap();
    let mut d = drive(root.path());
    d.pool_history_limit = 0;
    let io = FakeIo::default();
    stage(&d, &io, "f", b"first");
    d.sync_snapshots_with(&io).unwrap();
    let first = event_at(&d, "f");
    d.delete_snapshot_based_with("f", &Ancestry::Parents(vec![first]), &io)
        .unwrap();
    let deletion = d.state.lock().unwrap().pending.last().unwrap().clone();
    let captured = d.root.join("spool").join(&deletion.id).join("captured");
    assert!(
        captured.exists(),
        "original captured for the deletion's ancestry"
    );
    d.sync_snapshots_with(&io).unwrap();
    assert!(visible_bytes(&d, &io).is_empty());
}

#[test]
fn browse_adopts_recorded_history_limit_and_never_publishes() {
    let empty = FakeIo::default();
    let root_c = tempfile::tempdir().unwrap();
    let c = drive(root_c.path());
    assert!(c.seed_snapshots_with(&empty).unwrap().is_none());

    let root_a = tempfile::tempdir().unwrap();
    let mut a = drive(root_a.path());
    a.pool_history_limit = 1;
    let io = FakeIo::default();
    stage(&a, &io, "docs/nested/a.txt", b"alpha");
    stage(&a, &io, "top.bin", b"top-level");
    a.sync_snapshots_with(&io).unwrap();
    stage(&a, &io, "top.bin", b"top-level-2");
    a.sync_snapshots_with(&io).unwrap();

    // A fresh reader with the default limit cannot interpret the policy...
    let root_d = tempfile::tempdir().unwrap();
    let d = drive(root_d.path());
    assert!(d.pull_snapshots_with(&io).is_err());

    // ...but adopting the recorded one yields the writer's drive, read-only.
    let root_b = tempfile::tempdir().unwrap();
    let mut b = drive(root_b.path());
    let before = io.0.borrow().publications;
    let seeded = b.seed_snapshots_with(&io).unwrap();
    assert_eq!(seeded, Some(Some(1)));
    assert!(b.adopt_snapshot_policy(seeded));
    b.pull_snapshots_with(&io).unwrap();
    assert_eq!(io.0.borrow().publications, before);
    let sizes = |d: &VirtualDrive| -> BTreeMap<String, u64> {
        d.view()
            .unwrap()
            .into_iter()
            .map(|(p, r)| (p, r.size()))
            .collect()
    };
    assert_eq!(
        sizes(&b),
        BTreeMap::from([("docs/nested/a.txt".into(), 5), ("top.bin".into(), 11)])
    );
    assert_eq!(sizes(&b), sizes(&a));
}

#[test]
fn drive_retention_defers_snapshot_gc_until_versions_expire() {
    let root = tempfile::tempdir().unwrap();
    let mut d = drive(root.path());
    d.pool_history_limit = 0;
    d.history_retention = Some(crate::drive_history::model::Retention {
        trash_days: 30,
        keep_versions: 20,
        version_days: 90,
    });
    let io = FakeIo::default();
    for text in [b"version0", b"version1", b"version2"] {
        stage(&d, &io, "file", text);
        d.sync_snapshots_with(&io).unwrap();
    }
    // history_limit 0 alone would collect version0/1; retention keeps them.
    let live: BTreeSet<_> = io.0.borrow().payloads.values().cloned().collect();
    assert!(live.contains(b"version0".as_slice()) && live.contains(b"version1".as_slice()));
    let export = d.snapshot_export().unwrap();
    let times = BTreeMap::new();
    let history =
        crate::drive_history::source_v7::build(&export, &times, 0, BTreeSet::new()).unwrap();
    let restorable = history
        .revs
        .values()
        .filter(|r| r.content.as_ref().is_some_and(|c| c.restorable))
        .count();
    assert_eq!(restorable, 3);
    // Once every record is older than the version period, GC proceeds.
    d.history_retention.as_mut().unwrap().version_days = 1;
    let seen: BTreeMap<String, u64> = export
        .snapshots
        .keys()
        .chain(export.names.keys())
        .map(|id| (id.clone(), 0))
        .collect();
    durable_json(&d.root.join("drive-history-seen.json"), &seen).unwrap();
    d.sync_snapshots_with(&io).unwrap();
    let live: BTreeSet<_> = io.0.borrow().payloads.values().cloned().collect();
    assert!(!live.contains(b"version0".as_slice()) && !live.contains(b"version1".as_slice()));
    assert!(live.contains(b"version2".as_slice()));
}

#[test]
fn drive_retention_keeps_deleted_file_bytes_while_in_the_trash() {
    let root = tempfile::tempdir().unwrap();
    let mut d = drive(root.path());
    d.pool_history_limit = 0;
    let io = FakeIo::default();
    stage(&d, &io, "gone", b"deleted bytes");
    d.sync_snapshots_with(&io).unwrap();
    d.history_retention = Some(crate::drive_history::model::Retention::default());
    d.delete("gone").unwrap();
    d.sync_snapshots_with(&io).unwrap();
    d.sync_snapshots_with(&io).unwrap();
    let live: BTreeSet<_> = io.0.borrow().payloads.values().cloned().collect();
    assert!(live.contains(b"deleted bytes".as_slice()));
    let export = d.snapshot_export().unwrap();
    let history =
        crate::drive_history::source_v7::build(&export, &BTreeMap::new(), 10, BTreeSet::new())
            .unwrap();
    let trash = crate::drive_history::trash_list_for_tests(&history);
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].path, "/gone");
}
