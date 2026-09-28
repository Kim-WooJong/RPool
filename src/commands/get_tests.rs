use super::*;
use crate::manifest::{
    content_root_v1, content_root_v2, load_manifest_with_storage, recover_manifest_with_storage,
    verify_manifest_replicas_with_storage,
};
use crate::storage::error::{StorageError, StorageErrorKind};
use crate::storage::memory::faults::{Fault, FaultBackend, Operation, Rule};
use crate::storage::memory::MemoryBackend;
use crate::storage::reference::{BackendId, ObjectKey, ObjectRef};
use crate::storage::registry::BackendRegistry;
use crate::storage::traits::{OperationContext, StorageBackend, WriteOptions};

struct Fixture {
    memory: Arc<MemoryBackend>,
    manifest: Manifest,
    bindings: BTreeMap<String, ObjectRef>,
}
fn native(index: u32) -> ObjectKey {
    ObjectKey::new(format!("native/{index}")).unwrap()
}
fn put(memory: &MemoryBackend, key: &ObjectKey, bytes: &[u8]) {
    memory
        .write(
            &OperationContext::none(),
            key,
            &mut std::io::Cursor::new(bytes),
            &WriteOptions::default(),
        )
        .unwrap();
}
impl Fixture {
    fn new(coded: bool) -> Self {
        let id = BackendId::new("synthetic-only").unwrap();
        let memory = Arc::new(MemoryBackend::new(id.clone()));
        let coding = if coded {
            Some(Coding {
                algorithm: RS_ALGORITHM.into(),
                data_shards: 2,
                parity_shards: 2,
                stripe_size: 2,
            })
        } else {
            None
        };
        let payloads = vec![b"ABCD".to_vec(), b"EFGH".to_vec(), b"I".to_vec()];
        let mut objects = payloads.clone();
        if coded {
            let rs = ReedSolomon::new(2, 2).unwrap();
            for group in 0..2 {
                let mut blocks = vec![vec![0u8; 4]; 4];
                for slot in 0..2 {
                    if let Some(bytes) = payloads.get(group * 2 + slot) {
                        blocks[slot][..bytes.len()].copy_from_slice(bytes);
                    }
                }
                rs.encode(&mut blocks).unwrap();
                objects.extend_from_slice(&blocks[2..]);
            }
        }
        let mut bindings = BTreeMap::new();
        let mut shards = Vec::new();
        for (index, bytes) in objects.iter().enumerate() {
            let data = index < 3;
            let group = if data { index / 2 } else { (index - 3) / 2 };
            let slot = if data { index % 2 } else { 2 + (index - 3) % 2 };
            let object = format!("legacy:자료\\part:{index}%20");
            shards.push(Shard {
                index: index as u32,
                offset: if data { (index * 4) as u64 } else { 0 },
                size: bytes.len() as u64,
                remote: "legacy:".into(),
                object: object.clone(),
                blake3: blake3::hash(bytes).to_hex().to_string(),
                kind: if data {
                    ShardKind::Data
                } else {
                    ShardKind::Parity
                },
                group: group as u32,
                slot: slot as u16,
            });
            put(&memory, &native(index as u32), bytes);
            bindings.insert(object, ObjectRef::new(id.clone(), native(index as u32)));
        }
        let root = if coded {
            content_root_v2(9, 4, &coding, &shards)
        } else {
            content_root_v1(&shards)
        };
        let manifest = Manifest {
            version: if coded { 2 } else { 1 },
            archive_id: "fixture".into(),
            original_name: "input".into(),
            original_size: 9,
            shard_size: 4,
            created_unix: 0,
            content_root_blake3: root,
            coding,
            shards,
        };
        validate_manifest(&manifest).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let key = ObjectKey::new("manifest").unwrap();
        put(&memory, &key, &bytes);
        for raw in ["meta:manifest", "legacy:fixture/manifest.json"] {
            bindings.insert(raw.into(), ObjectRef::new(id.clone(), key.clone()));
        }
        Self {
            memory,
            manifest,
            bindings,
        }
    }
    fn reader(&self, rules: Vec<Rule>) -> StorageReader {
        let backend: Arc<dyn StorageBackend> = if rules.is_empty() {
            self.memory.clone()
        } else {
            Arc::new(FaultBackend::new(self.memory.clone(), rules).unwrap())
        };
        let mut registry = BackendRegistry::new();
        registry.register(backend).unwrap();
        StorageReader::from_registry(registry, self.bindings.clone(), OperationContext::none())
    }
    fn delete(&self, index: u32) {
        self.memory
            .delete(&OperationContext::none(), &native(index))
            .unwrap();
    }
}

#[test]
fn plain_restore_and_resume_use_only_injected_backend() {
    let f = Fixture::new(false);
    let reader = f.reader(vec![]);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("restored file");
    get_with_storage(&reader, "meta:manifest", &out, 2, 1).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"ABCDEFGHI");
    assert!(!append_suffix(&out, ".rpool.resume.json").exists());
    let state_path = prepare_output_and_state(&f.manifest, &out).unwrap();
    fs::write(&out, b"ABCDxxxxI").unwrap();
    let mut state: ResumeState = read_json(&state_path).unwrap();
    state.completed.extend([0, 1, 2, 999]);
    persist_restore_state(&state_path, &mut state).unwrap();
    f.delete(0);
    f.delete(2); // Verified resume ranges must not be downloaded again.
    get_with_storage(&reader, "meta:manifest", &out, 2, 1).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"ABCDEFGHI");
    assert!(!state_path.exists());
}

#[test]
fn erasure_handles_missing_corrupt_short_final_and_virtual_zero() {
    let f = Fixture::new(true);
    f.delete(0);
    f.delete(3); // Skip missing first parity; use second.
    put(&f.memory, &native(2), b"Z"); // Final short shard corrupt, final group has virtual zero slot.
    let reader = f.reader(vec![]);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    get_with_storage(&reader, "meta:manifest", &out, 1, 1).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"ABCDEFGHI");
    assert!(!append_suffix(&out, ".rpool.resume.json").exists());
}

#[test]
fn insufficient_parity_does_not_mark_lost_data_complete() {
    let f = Fixture::new(true);
    for index in [0, 3, 4] {
        f.delete(index)
    }
    let reader = f.reader(vec![]);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    assert!(get_with_storage(&reader, "meta:manifest", &out, 1, 1).is_err());
    let state: ResumeState = read_json(&append_suffix(&out, ".rpool.resume.json")).unwrap();
    assert!(!state.completed.contains(&0));
}

#[test]
fn auth_permission_cancel_and_configuration_never_trigger_parity_fallback() {
    for parity in [false, true] {
        for error in [
            StorageError::Authentication {
                detail: "synthetic".into(),
            },
            StorageError::PermissionDenied {
                path: "synthetic".into(),
            },
            StorageError::Cancelled {
                detail: "synthetic".into(),
            },
            StorageError::invalid_input("synthetic"),
            StorageError::unknown_outcome("synthetic"),
        ] {
            let kind = error.kind();
            let f = Fixture::new(true);
            if parity {
                f.delete(0)
            }
            let reader = f.reader(vec![Rule {
                operation: Operation::Read,
                call: if parity { 3 } else { 1 },
                fault: Fault::Error(error),
            }]);
            let temp = tempfile::tempdir().unwrap();
            let out = temp.path().join("out");
            let error = get_with_storage(&reader, "meta:manifest", &out, 1, 1).unwrap_err();
            assert_eq!(error.downcast_ref::<StorageError>().unwrap().kind(), kind);
            let state: ResumeState = read_json(&append_suffix(&out, ".rpool.resume.json")).unwrap();
            assert!(!state.completed.contains(&0));
        }
    }
}

#[test]
fn short_corrupt_and_extra_bytes_are_never_committed_to_resume() {
    for fault in [Fault::ShortRead(2), Fault::CorruptRead] {
        let f = Fixture::new(false);
        let reader = f.reader(vec![Rule {
            operation: Operation::Read,
            call: 1,
            fault,
        }]);
        let temp = tempfile::tempdir().unwrap();
        let out = temp.path().join("out");
        let error = get_with_storage(&reader, "meta:manifest", &out, 1, 1).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StorageError>().unwrap().kind(),
            StorageErrorKind::CorruptData
        );
        let state: ResumeState = read_json(&append_suffix(&out, ".rpool.resume.json")).unwrap();
        assert!(!state.completed.contains(&0));
    }
    let f = Fixture::new(false);
    put(&f.memory, &native(0), b"ABCD-extra");
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    assert!(get_with_storage(&f.reader(vec![]), "meta:manifest", &out, 1, 1).is_err());
    let state: ResumeState = read_json(&append_suffix(&out, ".rpool.resume.json")).unwrap();
    assert!(!state.completed.contains(&0));
}

#[test]
fn transient_read_retries_through_the_trait() {
    let f = Fixture::new(false);
    let reader = f.reader(vec![Rule {
        operation: Operation::Read,
        call: 1,
        fault: Fault::Error(StorageError::TransientIo {
            detail: "once".into(),
        }),
    }]);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    get_with_storage(&reader, "meta:manifest", &out, 1, 2).unwrap();
    assert_eq!(fs::read(out).unwrap(), b"ABCDEFGHI");
}

#[test]
fn manifest_load_recovery_and_replica_verification_preserve_format() {
    let f = Fixture::new(false);
    let reader = f.reader(vec![]);
    let before = serde_json::to_vec(&f.manifest).unwrap();
    let loaded = load_manifest_with_storage(&reader, "meta:manifest").unwrap();
    assert_eq!(serde_json::to_vec(&loaded).unwrap(), before);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("manifest.json");
    assert_eq!(
        recover_manifest_with_storage(&reader, "fixture", &["legacy:".into()], &out).unwrap(),
        "legacy:fixture/manifest.json"
    );
    let recovered: Manifest = read_json(&out).unwrap();
    assert_eq!(serde_json::to_vec(&recovered).unwrap(), before);
    let reports =
        verify_manifest_replicas_with_storage(&reader, &f.manifest, &["legacy:".into()]).unwrap();
    assert!(reports[0].healthy);
    for shard in &f.manifest.shards {
        reader.verify(shard, false).unwrap();
        reader.verify(shard, true).unwrap();
    }
}

#[test]
fn scan_and_probe_keep_missing_corruption_and_provider_error_distinct() {
    let f = Fixture::new(true);
    f.delete(0);
    put(&f.memory, &native(1), b"longer");
    put(&f.memory, &native(2), b"Z");
    let reader = f.reader(vec![Rule {
        operation: Operation::Stat,
        call: 4,
        fault: Fault::Error(StorageError::Authentication {
            detail: "synthetic".into(),
        }),
    }]);
    let (report, _) =
        crate::maintenance::scan_manifest_with_storage(&reader, &f.manifest, true, 1).unwrap();
    assert_eq!(
        (
            report.missing,
            report.bad_size,
            report.corrupt,
            report.errors
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(report.groups[0].status, "provider-error");
    let reader = f.reader(vec![Rule {
        operation: Operation::Read,
        call: 1,
        fault: Fault::Error(StorageError::not_found("vanished")),
    }]);
    assert!(matches!(
        reader.probe(&f.manifest.shards[4], true),
        Probe::Missing
    ));
}

#[test]
fn routing_configuration_errors_are_not_remote_loss() {
    let f = Fixture::new(false);
    for bindings in [BTreeMap::new(), f.bindings.clone()] {
        let reader = StorageReader::from_registry(
            BackendRegistry::new(),
            bindings,
            OperationContext::none(),
        );
        let error = reader.verify(&f.manifest.shards[0], true).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StorageError>().unwrap().kind(),
            StorageErrorKind::InvalidInput
        );
        assert!(!is_recoverable_loss(&error));
        assert!(matches!(
            reader.probe(&f.manifest.shards[0], false),
            Probe::Error(_)
        ));
    }
}

#[test]
fn metadata_bounds_short_reads_and_local_manifest_precedence() {
    let f = Fixture::new(false);
    let reader = f.reader(vec![Rule {
        operation: Operation::ReadAll,
        call: 1,
        fault: Fault::ShortRead(2),
    }]);
    assert_eq!(
        reader
            .read_metadata("meta:manifest")
            .unwrap_err()
            .downcast_ref::<StorageError>()
            .unwrap()
            .kind(),
        StorageErrorKind::CorruptData
    );
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("manifest file.json");
    fs::write(&local, serde_json::to_vec(&f.manifest).unwrap()).unwrap();
    let empty = StorageReader::from_registry(
        BackendRegistry::new(),
        BTreeMap::new(),
        OperationContext::none(),
    );
    assert_eq!(
        load_manifest_with_storage(&empty, local.to_str().unwrap())
            .unwrap()
            .archive_id,
        "fixture"
    );
    let excessive = vec![b' '; 64 * 1024 * 1024 + 1];
    put(&f.memory, &ObjectKey::new("manifest").unwrap(), &excessive);
    assert_eq!(
        f.reader(vec![])
            .read_metadata("meta:manifest")
            .unwrap_err()
            .downcast_ref::<StorageError>()
            .unwrap()
            .kind(),
        StorageErrorKind::InvalidInput
    );
}

#[test]
fn status_and_verify_command_cores_need_no_rclone_executable() {
    let f = Fixture::new(true);
    let reader = f.reader(vec![]);
    crate::commands::status::status_with_storage(
        &reader,
        "intentionally-missing-rclone-executable",
        "meta:manifest",
        2,
        false,
    )
    .unwrap();
    crate::commands::verify::verify_with_storage(&reader, "meta:manifest", true, 2).unwrap();
}

#[test]
fn exhausted_communication_failures_use_parity_and_skip_unavailable_parity() {
    for error in [
        StorageError::Timeout {
            detail: "synthetic".into(),
        },
        StorageError::TransientIo {
            detail: "synthetic".into(),
        },
        StorageError::RateLimited {
            detail: "synthetic".into(),
            retry_after: None,
        },
    ] {
        for fail_parity in [false, true] {
            let f = Fixture::new(true);
            if fail_parity {
                f.delete(0);
            }
            let reader = f.reader(vec![Rule {
                operation: Operation::Read,
                call: if fail_parity { 4 } else { 1 },
                fault: Fault::Error(error.clone()),
            }]);
            let temp = tempfile::tempdir().unwrap();
            let out = temp.path().join("out");
            get_with_storage(&reader, "meta:manifest", &out, 1, 1).unwrap();
            assert_eq!(fs::read(out).unwrap(), b"ABCDEFGHI");
        }
    }
}

#[test]
fn local_sink_failure_is_not_a_network_outage_or_retry() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new(true);
    let error = f
        .reader(vec![])
        .verified_read(&f.manifest.shards[0], &mut Broken)
        .unwrap_err();
    assert!(!crate::storage::reader::is_restore_unavailable(&error));
    assert!(crate::storage::scheduler::read_retry(&error, 1).is_none());
}

#[test]
fn two_missing_data_shards_restore_with_parallel_parity() {
    let f = Fixture::new(true);
    f.delete(0);
    f.delete(1);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    get_with_storage(&f.reader(vec![]), "meta:manifest", &out, 4, 1).unwrap();
    assert_eq!(fs::read(out).unwrap(), b"ABCDEFGHI");
}

#[test]
fn later_group_starts_while_first_group_is_blocked() {
    use crate::storage::memory::faults::Gate;
    let f = Fixture::new(true);
    let first = Gate::new();
    let later = Gate::new();
    let reader = f.reader(vec![
        Rule {
            operation: Operation::Read,
            call: 1,
            fault: Fault::PauseBefore(first.clone()),
        },
        Rule {
            operation: Operation::Read,
            call: 3,
            fault: Fault::PauseBefore(later.clone()),
        },
    ]);
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| get_with_storage(&reader, "meta:manifest", &out, 3, 1));
        first.reached.wait();
        later.reached.wait(); // third shard may start without first group finishing
        later.release.wait();
        first.release.wait();
        handle.join().unwrap().unwrap();
    });
    assert_eq!(fs::read(out).unwrap(), b"ABCDEFGHI");
}

#[test]
fn exhausted_retries_then_parity_preserves_resume_and_content() {
    let f = Fixture::new(true);
    let reader = f.reader(
        [1, 4]
            .into_iter()
            .map(|call| Rule {
                operation: Operation::Read,
                call,
                fault: Fault::Error(StorageError::Timeout {
                    detail: "synthetic outage".into(),
                }),
            })
            .collect(),
    );
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    get_with_storage(&reader, "meta:manifest", &out, 1, 2).unwrap();
    assert_eq!(fs::read(out).unwrap(), b"ABCDEFGHI");
}
