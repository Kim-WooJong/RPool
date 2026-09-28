use super::error::{StorageError, StorageErrorKind};
use super::memory::{
    faults::{Fault, FaultBackend, Operation, Rule},
    MemoryBackend,
};
use super::reader::StorageReader;
use super::reference::{BackendId, ObjectKey, ObjectRef};
use super::registry::BackendRegistry;
use super::traits::{OperationContext, StorageBackend, WriteOptions};
use super::writer::StorageWriter;
use crate::planning::{build_upload_plan, shard_from_plan};
use crate::prelude::*;
use crate::utils::{hash_file_range, remote_join};

struct Fixture {
    memory: Arc<MemoryBackend>,
    bindings: BTreeMap<String, ObjectRef>,
    plan: UploadPlan,
    temp: tempfile::TempDir,
    source: PathBuf,
}
impl Fixture {
    fn new(coded: bool) -> Self {
        let plan = build_upload_plan(
            "no-rclone",
            9,
            4,
            "archive".into(),
            vec!["a:".into(), "b:".into()],
            Placement::RoundRobin,
            coded.then(|| Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: 2,
                parity_shards: 1,
                stripe_size: 2,
            }),
        )
        .unwrap();
        let memory = Arc::new(MemoryBackend::new(BackendId::new("synthetic").unwrap()));
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"ABCDEFGHI").unwrap();
        let mut f = Self {
            memory,
            bindings: BTreeMap::new(),
            plan,
            temp,
            source,
        };
        for shard in f.plan.shards.clone() {
            f.bind(&shard.object);
            let relative =
                crate::utils::relative_remote_object(&shard.remote, &shard.object).unwrap();
            f.bind(&remote_join("c:", &relative));
        }
        for remote in ["a:", "b:", "c:"] {
            f.bind(&remote_join(remote, "archive/manifest.json"));
        }
        f
    }
    fn bind(&mut self, raw: &str) {
        if !self.bindings.contains_key(raw) {
            let key = ObjectKey::new(format!("object-{}", self.bindings.len())).unwrap();
            self.bindings
                .insert(raw.into(), ObjectRef::new(self.memory.id(), key));
        }
    }
    fn writer(&self, rules: Vec<Rule>) -> StorageWriter {
        let backend = Arc::new(FaultBackend::new(self.memory.clone(), rules).unwrap());
        let mut registry = BackendRegistry::new();
        registry.register(backend).unwrap();
        StorageWriter::synthetic(StorageReader::from_registry(
            registry,
            self.bindings.clone(),
            OperationContext::none(),
        ))
    }
    fn seed(&self, raw: &str, bytes: &[u8]) {
        self.memory
            .write(
                &OperationContext::none(),
                self.bindings[raw].key(),
                &mut std::io::Cursor::new(bytes),
                &WriteOptions::default(),
            )
            .unwrap();
    }
    fn bytes(&self, raw: &str) -> Vec<u8> {
        self.memory
            .read_all(&OperationContext::none(), self.bindings[raw].key(), None)
            .unwrap()
    }
    fn manifest(&self, writer: &StorageWriter) -> Manifest {
        let mut shards: Vec<Shard> = self
            .plan
            .shards
            .iter()
            .filter(|p| p.kind == ShardKind::Data)
            .map(|p| crate::storage::upload_one_data_shard(writer, &self.source, p, 2).unwrap())
            .collect();
        if let Some(coding) = &self.plan.coding {
            let temp = tempfile::tempdir().unwrap();
            for group in 0..crate::manifest::coding_group_count(3, coding.data_shards) {
                for item in crate::erasure::generate_parity_group(
                    &self.source,
                    &self.plan,
                    coding,
                    group as u32,
                    temp.path(),
                )
                .unwrap()
                {
                    let shard = crate::planning::shard_from_plan(&item.plan, item.blake3);
                    writer.write_file(&item.path, 0, &shard, 2).unwrap();
                    shards.push(shard);
                }
            }
        }
        shards.sort_by_key(|s| s.index);
        Manifest {
            version: 2,
            archive_id: "archive".into(),
            original_name: "source".into(),
            original_size: 9,
            shard_size: 4,
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(9, 4, &self.plan.coding, &shards),
            coding: self.plan.coding.clone(),
            shards,
        }
    }
}
fn error(kind: StorageErrorKind) -> StorageError {
    match kind {
        StorageErrorKind::Authentication => StorageError::Authentication {
            detail: "synthetic".into(),
        },
        StorageErrorKind::Timeout => StorageError::Timeout {
            detail: "synthetic".into(),
        },
        _ => StorageError::unknown_outcome("synthetic"),
    }
}

#[test]
fn data_and_parity_replace_same_size_corruption_and_reuse_only_hash_match() {
    let f = Fixture::new(true);
    for p in &f.plan.shards {
        f.seed(&p.object, &vec![b'X'; p.size as usize]);
    }
    let manifest = f.manifest(&f.writer(vec![]));
    for shard in &manifest.shards {
        f.writer(vec![]).reader().verify(shard, true).unwrap();
    }
    // Any write would now fail: all data and regenerated parity must be reusable.
    f.manifest(&f.writer(vec![Rule {
        operation: Operation::Write,
        call: 1,
        fault: Fault::Error(error(StorageErrorKind::Authentication)),
    }]));
}

#[test]
fn lost_write_acknowledgement_is_not_retried_or_recorded() {
    let f = Fixture::new(false);
    let p = &f.plan.shards[0];
    let writer = f.writer(vec![
        Rule {
            operation: Operation::Write,
            call: 1,
            fault: Fault::LoseResponse(None),
        },
        Rule {
            operation: Operation::Write,
            call: 2,
            fault: Fault::Error(error(StorageErrorKind::Authentication)),
        },
    ]);
    let err = crate::storage::upload_one_data_shard(&writer, &f.source, p, 5).unwrap_err();
    assert_eq!(
        err.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::UnknownOutcome
    );
    assert_eq!(f.bytes(&p.object), b"ABCD");
}

#[test]
fn failed_readback_never_retries_the_write_or_returns_completed_shard() {
    let f = Fixture::new(false);
    let writer = f.writer(vec![
        Rule {
            operation: Operation::Read,
            call: 1,
            fault: Fault::CorruptRead,
        },
        Rule {
            operation: Operation::Write,
            call: 2,
            fault: Fault::Error(error(StorageErrorKind::Authentication)),
        },
    ]);
    let err = crate::storage::upload_one_data_shard(&writer, &f.source, &f.plan.shards[0], 5)
        .unwrap_err();
    assert_eq!(
        err.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::CorruptData
    );
}

#[test]
fn operational_probe_error_never_authorizes_overwrite() {
    let f = Fixture::new(false);
    let p = &f.plan.shards[0];
    f.seed(&p.object, b"XXXX");
    let writer = f.writer(vec![Rule {
        operation: Operation::Stat,
        call: 1,
        fault: Fault::Error(error(StorageErrorKind::Authentication)),
    }]);
    assert!(crate::storage::upload_one_data_shard(&writer, &f.source, p, 3).is_err());
    assert_eq!(f.bytes(&p.object), b"XXXX");
}

#[test]
fn journal_revalidates_source_identity_and_preserves_state_on_operational_error() {
    let f = Fixture::new(false);
    let path = f.temp.path().join("journal");
    let manifest = f.manifest(&f.writer(vec![]));
    let mut journal = crate::journal::load_or_create_upload_journal(&path, &f.plan).unwrap();
    for shard in manifest.shards {
        journal.completed.insert(shard.index, shard);
    }
    let before = serde_json::to_vec(&journal).unwrap();
    let writer = f.writer(vec![Rule {
        operation: Operation::Stat,
        call: 2,
        fault: Fault::Error(error(StorageErrorKind::Timeout)),
    }]);
    assert!(
        crate::journal::validate_upload_journal(&writer, &f.source, &f.plan, &mut journal).is_err()
    );
    assert_eq!(serde_json::to_vec(&journal).unwrap(), before);
    journal.completed.get_mut(&1).unwrap().offset += 1;
    fs::write(&f.source, b"ZBCDEFGHI").unwrap();
    assert_eq!(
        crate::journal::validate_upload_journal(
            &f.writer(vec![]),
            &f.source,
            &f.plan,
            &mut journal
        )
        .unwrap(),
        2
    );
    assert_eq!(
        journal.completed.keys().copied().collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn captured_source_is_stable_after_original_changes_and_expected_hash_is_enforced() {
    let f = Fixture::new(false);
    let snapshot = super::source::UploadSource::capture(&f.source, 9).unwrap();
    fs::write(&f.source, b"XXXXXXXXX").unwrap();
    assert_eq!(fs::read(snapshot.path()).unwrap(), b"ABCDEFGHI");
    let shard = shard_from_plan(
        &f.plan.shards[0],
        blake3::hash(b"ABCD").to_hex().to_string(),
    );
    assert!(f
        .writer(vec![])
        .write_file(&f.source, 0, &shard, 2)
        .is_err());
    assert!(f
        .memory
        .stat(&OperationContext::none(), f.bindings[&shard.object].key())
        .is_err());
    f.writer(vec![])
        .write_file(&snapshot.path(), 0, &shard, 2)
        .unwrap();
}

#[test]
fn repair_is_injected_verified_and_dry_run_does_not_mutate() {
    let f = Fixture::new(true);
    let manifest = f.manifest(&f.writer(vec![]));
    let bad = &manifest.shards[0];
    f.seed(&bad.object, b"XXXX");
    let writer = f.writer(vec![]);
    let (_, probes) =
        crate::maintenance::scan_manifest_with_storage(writer.reader(), &manifest, true, 1)
            .unwrap();
    assert_eq!(
        crate::maintenance::repair_manifest_with_storage(
            &writer, &manifest, &probes, 2, true, None
        )
        .unwrap(),
        1
    );
    assert_eq!(f.bytes(&bad.object), b"XXXX");
    assert_eq!(
        crate::maintenance::repair_manifest_with_storage(
            &writer, &manifest, &probes, 2, false, None
        )
        .unwrap(),
        1
    );
    assert_eq!(f.bytes(&bad.object), b"ABCD");
}

#[test]
fn migration_replica_failure_retains_all_source_shards() {
    let f = Fixture::new(false);
    let manifest = f.manifest(&f.writer(vec![]));
    let count = manifest.shards.iter().filter(|s| s.remote == "a:").count();
    let writer = f.writer(vec![Rule {
        operation: Operation::Write,
        call: count as u64 + 1,
        fault: Fault::Error(error(StorageErrorKind::Authentication)),
    }]);
    let output = f.temp.path().join("migrated.json");
    assert!(crate::provider::drain_manifest_with_storage(
        &writer, &manifest, "a:", "c:", &output, 1, 2, false, true, false
    )
    .is_err());
    assert!(output.exists()); // Existing local-publication-before-replicas semantics.
    for shard in &manifest.shards {
        f.writer(vec![]).reader().verify(shard, true).unwrap();
    }
    assert!(f
        .memory
        .stat(
            &OperationContext::none(),
            f.bindings["c:archive/manifest.json"].key()
        )
        .is_err());
}

#[test]
fn destructive_drain_rejects_virtual_archives_before_any_copy() {
    let f = Fixture::new(false);
    let mut manifest = f.manifest(&f.writer(vec![]));
    for prefix in ["virtual-", "peer-v7-"] {
        manifest.archive_id = format!("{prefix}{}", "a".repeat(64));
        let output = f.temp.path().join("must-not-publish.json");
        let error = crate::provider::drain_manifest_with_storage(
            &f.writer(vec![]),
            &manifest,
            "a:",
            "c:",
            &output,
            1,
            2,
            false,
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("immutable shared revisions"));
        assert!(!output.exists());
        for shard in &manifest.shards {
            f.writer(vec![]).reader().verify(shard, true).unwrap();
        }
    }
}

#[test]
fn migration_copy_failure_never_publishes_or_deletes_and_dry_run_is_read_only() {
    let f = Fixture::new(false);
    let manifest = f.manifest(&f.writer(vec![]));
    let writer = f.writer(vec![Rule {
        operation: Operation::Write,
        call: 1,
        fault: Fault::Error(error(StorageErrorKind::Authentication)),
    }]);
    let output = f.temp.path().join("migrated.json");
    crate::provider::drain_manifest_with_storage(
        &writer, &manifest, "a:", "c:", &output, 1, 1, true, true, false,
    )
    .unwrap();
    assert!(!output.exists());
    assert!(crate::provider::drain_manifest_with_storage(
        &writer, &manifest, "a:", "c:", &output, 1, 1, false, true, false
    )
    .is_err());
    assert!(!output.exists());
    for shard in &manifest.shards {
        f.writer(vec![]).reader().verify(shard, true).unwrap();
    }
}

#[test]
fn successful_migration_verifies_replicas_before_deleting_sources() {
    let f = Fixture::new(false);
    let manifest = f.manifest(&f.writer(vec![]));
    let output = f.temp.path().join("migrated.json");
    let writer = f.writer(vec![]);
    let updated = crate::provider::drain_manifest_with_storage(
        &writer, &manifest, "a:", "c:", &output, 1, 1, false, true, false,
    )
    .unwrap();
    for shard in &updated.shards {
        writer.reader().verify(shard, true).unwrap();
    }
    for shard in manifest.shards.iter().filter(|s| s.remote == "a:") {
        assert!(f
            .memory
            .stat(&OperationContext::none(), f.bindings[&shard.object].key())
            .is_err());
    }
    for remote in ["b:", "c:"] {
        let replica: Manifest =
            serde_json::from_slice(&f.bytes(&remote_join(remote, "archive/manifest.json")))
                .unwrap();
        assert_eq!(
            serde_json::to_vec(&replica).unwrap(),
            serde_json::to_vec(&updated).unwrap()
        );
    }
}

#[test]
fn put_command_core_writes_verified_archive_and_can_rebuild_changed_source() {
    let mut f = Fixture::new(false);
    f.bind("a:archive/shards/00000000.bin");
    let writer = f.writer(vec![]);
    for bytes in [b"ABCDEFGHI", b"123456789"] {
        fs::write(&f.source, bytes).unwrap();
        crate::commands::put_with_storage(
            &writer,
            "no-rclone",
            &f.source,
            vec!["a:".into()],
            1,
            1,
            Placement::RoundRobin,
            2,
            2,
            0,
            Some("archive".into()),
            None,
        )
        .unwrap();
        let local = crate::utils::append_suffix(&f.source, ".rpool.json");
        let manifest: Manifest = crate::utils::read_json(&local).unwrap();
        assert_eq!(
            manifest.shards[0].blake3,
            hash_file_range(&f.source, 0, 9).unwrap()
        );
        writer.reader().verify(&manifest.shards[0], true).unwrap();
        assert!(!crate::utils::append_suffix(&f.source, ".rpool.upload.state.json").exists());
    }
}

#[test]
fn command_dry_runs_preserve_local_integrity_snapshot_and_remote_bytes() {
    let f = Fixture::new(true);
    let writer = f.writer(vec![]);
    let manifest = f.manifest(&writer);
    let local = f.temp.path().join("manifest.json");
    fs::write(&local, serde_json::to_vec(&manifest).unwrap()).unwrap();
    f.seed(&manifest.shards[0].object, b"XXXX");
    let snapshot_path = crate::config::integrity_snapshot_path().unwrap();
    let before = fs::read(&snapshot_path).ok();
    crate::commands::repair_with_storage(
        &writer,
        local.to_str().unwrap(),
        true,
        1,
        1,
        true,
        vec![],
    )
    .unwrap();
    crate::commands::scrub_with_storage(
        &writer,
        local.to_str().unwrap(),
        false,
        true,
        true,
        1,
        1,
        false,
    )
    .unwrap();
    assert_eq!(fs::read(&snapshot_path).ok(), before);
    assert_eq!(f.bytes(&manifest.shards[0].object), b"XXXX");
}

#[test]
fn repair_readback_corruption_is_not_success() {
    let f = Fixture::new(true);
    let manifest = f.manifest(&f.writer(vec![]));
    f.seed(&manifest.shards[0].object, b"XXXX");
    let (_, probes) = crate::maintenance::scan_manifest_with_storage(
        f.writer(vec![]).reader(),
        &manifest,
        true,
        1,
    )
    .unwrap();
    // Two healthy downloads, corrupted existing destination check, then readback.
    let writer = f.writer(vec![Rule {
        operation: Operation::Read,
        call: 4,
        fault: Fault::CorruptRead,
    }]);
    let err = crate::maintenance::repair_manifest_with_storage(
        &writer, &manifest, &probes, 1, false, None,
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::CorruptData
    );
}

#[test]
fn migration_replica_readback_failure_preserves_sources() {
    let f = Fixture::new(false);
    let manifest = f.manifest(&f.writer(vec![]));
    // Two copies: source + destination readback + caller verify, then replica readback.
    let writer = f.writer(vec![Rule {
        operation: Operation::Read,
        call: 7,
        fault: Fault::CorruptRead,
    }]);
    let err = crate::provider::drain_manifest_with_storage(
        &writer,
        &manifest,
        "a:",
        "c:",
        &f.temp.path().join("out"),
        1,
        1,
        false,
        true,
        false,
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::CorruptData
    );
    for shard in &manifest.shards {
        f.writer(vec![]).reader().verify(shard, true).unwrap();
    }
}

#[test]
fn failed_put_does_not_publish_manifest_or_mark_unverified_data_complete() {
    let f = Fixture::new(false);
    let writer = f.writer(vec![Rule {
        operation: Operation::Read,
        call: 1,
        fault: Fault::CorruptRead,
    }]);
    assert!(crate::commands::put_with_storage(
        &writer,
        "no-rclone",
        &f.source,
        vec!["a:".into()],
        1,
        1,
        Placement::RoundRobin,
        1,
        2,
        0,
        Some("archive".into()),
        None
    )
    .is_err());
    let local = crate::utils::append_suffix(&f.source, ".rpool.json");
    assert!(!local.exists());
    let journal: UploadJournal = crate::utils::read_json(&crate::utils::append_suffix(
        &f.source,
        ".rpool.upload.state.json",
    ))
    .unwrap();
    assert!(journal.completed.is_empty());
    assert!(f
        .memory
        .stat(
            &OperationContext::none(),
            f.bindings["a:archive/manifest.json"].key()
        )
        .is_err());
}

#[test]
fn interrupted_put_resumes_changed_source_consistently() {
    let f = Fixture::new(false);
    let writer = f.writer(vec![Rule {
        operation: Operation::Write,
        call: 2,
        fault: Fault::Error(error(StorageErrorKind::Authentication)),
    }]);
    assert!(crate::commands::put_with_storage(
        &writer,
        "no-rclone",
        &f.source,
        vec!["a:".into()],
        1,
        1,
        Placement::RoundRobin,
        1,
        2,
        0,
        Some("archive".into()),
        None
    )
    .is_err());
    let journal_path = crate::utils::append_suffix(&f.source, ".rpool.upload.state.json");
    let journal: UploadJournal = crate::utils::read_json(&journal_path).unwrap();
    assert_eq!(journal.completed.len(), 1);
    fs::write(&f.source, b"123456789").unwrap();
    crate::commands::put_with_storage(
        &f.writer(vec![]),
        "no-rclone",
        &f.source,
        vec!["a:".into()],
        1,
        1,
        Placement::RoundRobin,
        1,
        2,
        0,
        Some("archive".into()),
        None,
    )
    .unwrap();
    assert_eq!(f.bytes("a:archive/shards/00000000.bin"), b"123456789");
    assert!(!journal_path.exists());
}

#[test]
fn copy_between_distinct_backends_moves_verified_logical_bytes() {
    let f = Fixture::new(false);
    let p = &f.plan.shards[0];
    f.seed(&p.object, b"ABCD");
    let other = Arc::new(MemoryBackend::new(BackendId::new("other").unwrap()));
    let target_key = ObjectKey::new("destination").unwrap();
    let mut bindings = f.bindings.clone();
    bindings.insert(
        "other:target".into(),
        ObjectRef::new(other.id(), target_key.clone()),
    );
    let mut registry = BackendRegistry::new();
    registry.register(f.memory.clone()).unwrap();
    registry.register(other.clone()).unwrap();
    let writer = StorageWriter::synthetic(StorageReader::from_registry(
        registry,
        bindings,
        OperationContext::none(),
    ));
    let shard = shard_from_plan(p, blake3::hash(b"ABCD").to_hex().to_string());
    writer.copy_verified(&shard, "other:target", 1).unwrap();
    assert_eq!(
        other
            .read_all(&OperationContext::none(), &target_key, None)
            .unwrap(),
        b"ABCD"
    );
    assert_eq!(f.bytes(&p.object), b"ABCD");
}

#[test]
fn multi_group_scheduled_upload_and_two_shard_recovery_roundtrip() {
    let mut f = Fixture::new(false);
    let bytes: Vec<u8> = (0..5 * 1024 * 1024 + 19).map(|i| (i % 251) as u8).collect();
    fs::write(&f.source, &bytes).unwrap();
    let remotes = vec!["a:".into(), "b:".into(), "c:".into()];
    f.plan = build_upload_plan(
        "no-rclone",
        bytes.len() as u64,
        1024 * 1024,
        "archive".into(),
        remotes.clone(),
        Placement::RoundRobin,
        Some(Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 2,
            parity_shards: 2,
            stripe_size: EC_STRIPE_SIZE,
        }),
    )
    .unwrap();
    for shard in f.plan.shards.clone() {
        f.bind(&shard.object);
    }
    let writer = f.writer(vec![]);
    crate::commands::put_with_storage(
        &writer,
        "no-rclone",
        &f.source,
        remotes,
        1,
        4,
        Placement::RoundRobin,
        2,
        2,
        2,
        Some("archive".into()),
        None,
    )
    .unwrap();
    let manifest_path = crate::utils::append_suffix(&f.source, ".rpool.json");
    let manifest: Manifest = crate::utils::read_json(&manifest_path).unwrap();
    assert_eq!(manifest.shards.len(), 12);
    for shard in manifest
        .shards
        .iter()
        .filter(|s| s.kind == ShardKind::Data && s.group == 0)
    {
        f.memory
            .delete(&OperationContext::none(), f.bindings[&shard.object].key())
            .unwrap();
    }
    let restored = f.temp.path().join("restored");
    crate::commands::get_with_storage(
        writer.reader(),
        manifest_path.to_str().unwrap(),
        &restored,
        4,
        2,
    )
    .unwrap();
    assert_eq!(fs::read(restored).unwrap(), bytes);
}

#[test]
fn scheduler_never_retries_acknowledged_upload_after_readback_timeout() {
    let f = Fixture::new(false);
    let writer = f.writer(vec![Rule {
        operation: Operation::Read,
        call: 1,
        fault: Fault::Error(StorageError::Timeout {
            detail: "readback".into(),
        }),
    }]);
    let error = crate::storage::upload_one_data_shard(&writer, &f.source, &f.plan.shards[0], 1)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<StorageError>().unwrap().kind(),
        StorageErrorKind::Timeout
    );
    assert!(super::writer::upload_retry(&error, 1).is_none());
}
