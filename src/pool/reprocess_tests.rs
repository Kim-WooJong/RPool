use super::*;

#[test]
fn estimate_includes_full_parity_for_partial_group() {
    let target = PoolDefinition {
        shard_mib: 1,
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
            remotes: vec!["a:".into()],
            shard_mib: 1,
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
