use super::*;

#[test]
fn recovery_lookup_requires_exact_completed_source_and_replacement_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("plan.json");
    let source = Manifest {
        version: 2,
        archive_id: "original".into(),
        original_name: "empty".into(),
        original_size: 0,
        shard_size: 1048576,
        created_unix: 0,
        content_root_blake3: crate::manifest::content_root_v2(0, 1048576, &None, &[]),
        coding: None,
        shards: vec![],
    };
    validate_manifest(&source).unwrap();
    let entry = ReprocessEntry {
        source: "old:unavailable/manifest.json".into(),
        fingerprint: manifest_fingerprint(&source).unwrap(),
        manifest: source.clone(),
    };
    let plan = ReprocessPlan {
        version: 2,
        operation_id: "test-operation".into(),
        plan_path: path.clone(),
        target: PoolDefinition {
            max_object_bytes: None,
            remotes: vec!["healthy:".into()],
            ..Default::default()
        },
        entries: vec![entry.clone()],
        input_bytes: 0,
        new_storage_bytes: 0,
        download_bytes: 0,
        upload_bytes: 0,
        estimated_seconds: None,
        estimate_note: String::new(),
        change_summary: vec![],
    };
    save_new(&path, &plan).unwrap();
    save_new(
        &temp.path().join("plan.fingerprint.json"),
        &blake3::hash(&fs::read(&path).unwrap()).to_hex().to_string(),
    )
    .unwrap();
    assert!(completed_reprocess_replacements(&path).unwrap().is_empty());
    let mut replacement = source;
    replacement.archive_id = "reprocess-new-independent".into();
    let manifest_path = temp.path().join("replacement.json");
    save_new(&manifest_path, &replacement).unwrap();
    let completion = temp.path().join("completed-00000000.json");
    record_completion(&completion, &entry, &manifest_path).unwrap();
    let pairs = completed_reprocess_replacements(&path).unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].0.archive_id, "original");
    assert_eq!(pairs[0].1.archive_id, "reprocess-new-independent");
    replacement.original_name = "changed".into();
    fs::write(&manifest_path, serde_json::to_vec(&replacement).unwrap()).unwrap();
    assert!(completed_reprocess_replacements(&path).is_err());
}

#[test]
fn estimate_includes_full_parity_for_partial_group() {
    let target = PoolDefinition {
        max_object_bytes: None,
        shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
        data_shards: 3,
        parity_shards: 2,
        ..Default::default()
    };
    assert_eq!(storage_bytes(1, &target).unwrap(), 1 + 2 * 1048576);
    assert_eq!(
        storage_bytes(3 * 1048576 + 1, &target).unwrap(),
        7 * 1048576 + 1
    );
    assert!(storage_bytes(0, &target).is_err());
    let plain = PoolDefinition {
        max_object_bytes: None,
        parity_shards: 0,
        ..target.clone()
    };
    assert_eq!(storage_bytes(0, &plain).unwrap(), 0);
    assert_eq!(storage_bytes(123, &plain).unwrap(), 123);
}

#[test]
fn fingerprint_detects_source_layout_change() {
    let mut manifest = Manifest {
        version: 2,
        archive_id: "original".into(),
        original_name: "file".into(),
        original_size: 0,
        shard_size: 1048576,
        created_unix: 0,
        content_root_blake3: "root".into(),
        coding: None,
        shards: vec![],
    };
    let fingerprint = manifest_fingerprint(&manifest).unwrap();
    manifest.original_name = "changed".into();
    assert_ne!(fingerprint, manifest_fingerprint(&manifest).unwrap());
}

#[test]
fn receipts_are_create_only_and_ids_are_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    save_new(&path, &"original").unwrap();
    assert!(save_new(&path, &"overwrite").is_err());
    assert_eq!(read_json::<String>(&path).unwrap(), "original");
    assert_ne!(random_id().unwrap(), random_id().unwrap());
}

use crate::storage::{
    error::StorageError,
    memory::{
        faults::{Fault, FaultBackend, Operation, Rule},
        MemoryBackend,
    },
    reader::StorageReader,
    reference::{BackendId, ObjectKey, ObjectRef},
    registry::BackendRegistry,
    traits::{OperationContext, StorageBackend},
};

struct ConversionFixture {
    memory: Arc<MemoryBackend>,
    bindings: BTreeMap<String, ObjectRef>,
    dir: tempfile::TempDir,
    bytes: Vec<u8>,
    original: ReprocessEntry,
}
impl ConversionFixture {
    fn new() -> Self {
        let memory = Arc::new(MemoryBackend::new(BackendId::new("conversion").unwrap()));
        let bytes: Vec<u8> = (0..(1048576 + 19)).map(|i| (i % 251) as u8).collect();
        let mut bindings = BTreeMap::new();
        for (id, coded) in [
            ("original", false),
            ("coded", true),
            ("plain", false),
            ("failed", true),
        ] {
            let plan = crate::planning::build_upload_plan(
                "no-rclone",
                bytes.len() as u64,
                1048576,
                id.into(),
                vec!["a:".into()],
                Placement::RoundRobin,
                coded.then(|| Coding {
                    algorithm: RS_ALGORITHM.into(),
                    data_shards: 2,
                    parity_shards: 1,
                    stripe_size: EC_STRIPE_SIZE,
                }),
            )
            .unwrap();
            for object in plan
                .shards
                .into_iter()
                .map(|s| s.object)
                .chain(std::iter::once(format!("a:{id}/manifest.json")))
            {
                let key = ObjectKey::new(format!("object-{}", bindings.len())).unwrap();
                bindings.insert(object, ObjectRef::new(memory.id(), key));
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original-source");
        fs::write(&source, &bytes).unwrap();
        let writer = Self::writer_for(&memory, &bindings, vec![]);
        crate::commands::put_with_storage(
            &writer,
            "no-rclone",
            &source,
            vec!["a:".into()],
            1,
            1,
            Placement::RoundRobin,
            1,
            2,
            0,
            Some("original".into()),
            None,
        )
        .unwrap();
        let path = append_suffix(&source, ".rpool.json");
        let mut manifest: Manifest = read_json(&path).unwrap();
        // Metadata must round-trip exactly, including names unsafe on Windows or Unix.
        manifest.original_name = "../../CON:*? archive. ".into();
        let original = ReprocessEntry {
            source: path.to_string_lossy().into(),
            fingerprint: manifest_fingerprint(&manifest).unwrap(),
            manifest,
        };
        Self {
            memory,
            bindings,
            dir,
            bytes,
            original,
        }
    }
    fn writer_for(
        memory: &Arc<MemoryBackend>,
        bindings: &BTreeMap<String, ObjectRef>,
        rules: Vec<Rule>,
    ) -> StorageWriter {
        let mut registry = BackendRegistry::new();
        registry
            .register(Arc::new(FaultBackend::new(memory.clone(), rules).unwrap()))
            .unwrap();
        StorageWriter::synthetic(StorageReader::from_registry(
            registry,
            bindings.clone(),
            OperationContext::none(),
        ))
    }
    fn writer(&self, rules: Vec<Rule>) -> StorageWriter {
        Self::writer_for(&self.memory, &self.bindings, rules)
    }
    fn target(coded: bool) -> PoolDefinition {
        PoolDefinition {
            max_object_bytes: None,
            remotes: vec!["a:".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
            workers: 1,
            retries: 1,
            placement: Placement::RoundRobin,
            data_shards: 2,
            parity_shards: usize::from(coded),
        }
    }
    fn snapshot_original(&self) -> Vec<Vec<u8>> {
        self.bindings
            .iter()
            .filter(|(raw, _)| raw.starts_with("a:original/"))
            .map(|(_, object)| {
                self.memory
                    .read_all(&OperationContext::none(), object.key(), None)
                    .unwrap()
            })
            .collect()
    }
}

#[test]
fn conversion_plain_to_rs_to_plain_preserves_content_metadata_and_originals() {
    let f = ConversionFixture::new();
    let original_bytes = f.snapshot_original();
    let writer = f.writer(vec![]);
    let mut entry = f.original.clone();
    for (id, coded) in [("coded", true), ("plain", false)] {
        let item = f.dir.path().join(id);
        private_dir(&item).unwrap();
        convert_one(
            &writer,
            "no-rclone",
            &entry,
            &ConversionFixture::target(coded),
            &item,
            id,
        )
        .unwrap();
        let path = item.join("manifest.json");
        let manifest: Manifest = read_json(&path).unwrap();
        assert_eq!(manifest.coding.is_some(), coded);
        assert_eq!(manifest.original_name, f.original.manifest.original_name);
        let restored = item.join("verified-content");
        crate::commands::get_with_storage(
            writer.reader(),
            &path.to_string_lossy(),
            &restored,
            1,
            1,
        )
        .unwrap();
        assert_eq!(fs::read(restored).unwrap(), f.bytes);
        entry = ReprocessEntry {
            source: path.to_string_lossy().into(),
            fingerprint: manifest_fingerprint(&manifest).unwrap(),
            manifest,
        };
    }
    assert_eq!(f.snapshot_original(), original_bytes);
}

#[test]
fn failed_upload_preserves_original_and_does_not_publish_success_manifest() {
    let f = ConversionFixture::new();
    let original_bytes = f.snapshot_original();
    let writer = f.writer(vec![Rule {
        operation: Operation::Write,
        call: 1,
        fault: Fault::Error(StorageError::Authentication {
            detail: "injected upload denial".into(),
        }),
    }]);
    let item = f.dir.path().join("failed-attempt");
    private_dir(&item).unwrap();
    assert!(convert_one(
        &writer,
        "no-rclone",
        &f.original,
        &ConversionFixture::target(true),
        &item,
        "failed"
    )
    .is_err());
    assert!(!item.join("manifest.json").exists());
    assert_eq!(f.snapshot_original(), original_bytes);
    for shard in &f.original.manifest.shards {
        f.writer(vec![]).reader().verify(shard, true).unwrap();
    }
}

#[test]
fn plan_lock_excludes_concurrent_execution_and_recovers_after_owner_release() {
    let dir = tempfile::tempdir().unwrap();
    let owner = lock_plan(dir.path()).unwrap();
    assert!(lock_plan(dir.path()).is_err());
    drop(owner);
    // The stale filename is intentionally retained; ownership is kernel-managed.
    assert!(dir.path().join("execution.lock").exists());
    assert!(lock_plan(dir.path()).is_ok());
}

#[test]
fn partial_completion_revalidates_without_writes_and_failed_item_retries_safely() {
    let f = ConversionFixture::new();
    let original = f.snapshot_original();
    let completed = f.dir.path().join("completed-00000000.json");
    let first = f.dir.path().join("first");
    private_dir(&first).unwrap();
    convert_one(
        &f.writer(vec![]),
        "no-rclone",
        &f.original,
        &ConversionFixture::target(true),
        &first,
        "coded",
    )
    .unwrap();
    record_completion(&completed, &f.original, &first.join("manifest.json")).unwrap();
    let denied = f.writer(vec![Rule {
        operation: Operation::Write,
        call: 1,
        fault: Fault::Error(StorageError::Authentication {
            detail: "must not write completed item".into(),
        }),
    }]);
    verify_completion(&denied, &completed, f.dir.path(), &f.original, 1).unwrap();
    let interrupted = f.dir.path().join("interrupted");
    private_dir(&interrupted).unwrap();
    assert!(convert_one(
        &denied,
        "no-rclone",
        &f.original,
        &ConversionFixture::target(true),
        &interrupted,
        "failed"
    )
    .is_err());
    assert!(!interrupted.join("manifest.json").exists());
    assert!(!f.dir.path().join("completed-00000001.json").exists());
    let retry = f.dir.path().join("retry");
    private_dir(&retry).unwrap();
    convert_one(
        &f.writer(vec![]),
        "no-rclone",
        &f.original,
        &ConversionFixture::target(false),
        &retry,
        "plain",
    )
    .unwrap();
    record_completion(
        &f.dir.path().join("completed-00000001.json"),
        &f.original,
        &retry.join("manifest.json"),
    )
    .unwrap();
    assert_eq!(f.snapshot_original(), original);
}

#[test]
fn corrupt_receipt_or_completed_manifest_never_counts_as_ready() {
    let f = ConversionFixture::new();
    let first = f.dir.path().join("first");
    private_dir(&first).unwrap();
    convert_one(
        &f.writer(vec![]),
        "no-rclone",
        &f.original,
        &ConversionFixture::target(true),
        &first,
        "coded",
    )
    .unwrap();
    let receipt = f.dir.path().join("completed.json");
    record_completion(&receipt, &f.original, &first.join("manifest.json")).unwrap();
    let denied = f.writer(vec![Rule {
        operation: Operation::Read,
        call: 1,
        fault: Fault::Error(StorageError::Authentication {
            detail: "offline replacement".into(),
        }),
    }]);
    assert!(verify_completion(&denied, &receipt, f.dir.path(), &f.original, 1).is_err());
    fs::write(first.join("manifest.json"), b"truncated").unwrap();
    assert!(validate_completion(&receipt, f.dir.path(), &f.original).is_err());
    fs::write(&receipt, b"truncated receipt").unwrap();
    assert!(validate_completion(&receipt, f.dir.path(), &f.original).is_err());
}

#[test]
fn execution_only_policy_is_not_reported_as_layout_change() {
    let f = ConversionFixture::new();
    let mut target = ConversionFixture::target(false);
    target.workers = 12;
    target.retries = 8;
    target.data_shards = 10; // K has no coding effect while M=0.
    let notes = describe_changes(&[f.original], &target).join("\n");
    assert!(notes.contains("shard size changed: false; K/M coding changed: false"));
    assert!(notes.contains("execution-only knobs"));
    assert!(notes.contains("not minimum movement"));
}

#[test]
fn saved_plan_load_accepts_legacy_and_rejects_tampering() {
    let f = ConversionFixture::new();
    let path = f.dir.path().join("plan.json");
    let plan = ReprocessPlan {
        version: 1,
        operation_id: "legacy".into(),
        plan_path: path.clone(),
        target: ConversionFixture::target(false),
        entries: vec![f.original],
        input_bytes: 1,
        new_storage_bytes: 1,
        download_bytes: 3,
        upload_bytes: 1,
        estimated_seconds: None,
        estimate_note: "legacy estimate".into(),
        change_summary: vec![],
    };
    let mut legacy = serde_json::to_value(plan).unwrap();
    legacy.as_object_mut().unwrap().remove("change_summary");
    save_new(&path, &legacy).unwrap();
    save_new(
        &f.dir.path().join("plan.fingerprint.json"),
        &blake3::hash(&fs::read(&path).unwrap()).to_hex().to_string(),
    )
    .unwrap();
    assert!(load_plan(&path).unwrap().change_summary.is_empty());
    fs::write(&path, b"tampered").unwrap();
    assert!(load_plan(&path).is_err());
}
