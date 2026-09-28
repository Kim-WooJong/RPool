use super::*;
use crate::models::{Manifest, ShardKind};
use crate::storage::{
    memory::MemoryBackend,
    reader::StorageReader,
    reference::{BackendId, ObjectKey, ObjectRef},
    registry::BackendRegistry,
    traits::*,
    writer::StorageWriter,
};
use std::{collections::BTreeMap, sync::Arc};
const FIXTURES: [&[u8]; 3] = [
    include_bytes!("fixtures/v1.json"),
    include_bytes!("fixtures/v2-plain.json"),
    include_bytes!("fixtures/v2-rs.json"),
];
fn setup() -> (Arc<MemoryBackend>, StorageWriter) {
    let b = Arc::new(MemoryBackend::new(BackendId::new("fixture").unwrap()));
    let mut registry = BackendRegistry::new();
    registry.register(b.clone()).unwrap();
    let bindings = BTreeMap::from([
        (
            "src:golden/manifest.json".into(),
            ObjectRef::new(b.id(), ObjectKey::new("source").unwrap()),
        ),
        (
            "dst:golden/manifest.json".into(),
            ObjectRef::new(b.id(), ObjectKey::new("replica").unwrap()),
        ),
        (
            "Crypt:경로//A%20B\\x".into(),
            ObjectRef::new(b.id(), ObjectKey::new("data").unwrap()),
        ),
    ]);
    (
        b,
        StorageWriter::synthetic(StorageReader::from_registry(
            registry,
            bindings,
            OperationContext::none(),
        )),
    )
}
fn put(b: &MemoryBackend, k: &str, bytes: &[u8]) {
    b.write(
        &OperationContext::none(),
        &ObjectKey::new(k).unwrap(),
        &mut &bytes[..],
        &WriteOptions::default(),
    )
    .unwrap();
}
#[test]
fn fixed_roots_defaults_and_runtime_addresses() {
    for bytes in FIXTURES {
        let m: Manifest = serde_json::from_slice(bytes).unwrap();
        validate_manifest(&m).unwrap();
        assert_eq!(m.shards[0].kind, ShardKind::Data);
        assert_eq!(m.shards[0].group, 0);
        assert_eq!(m.shards[0].slot, 0);
        let (b, w) = setup();
        put(&b, "data", b"abc");
        w.reader().verify(&m.shards[0], true).unwrap();
        let saved = serde_json::to_value(&m).unwrap();
        assert_eq!(saved["shards"][0]["object"], "Crypt:경로//A%20B\\x");
        for field in [
            "backend_id",
            "object_key",
            "generation",
            "volume_id",
            "failure_domain_id",
            "capacity_domain_id",
        ] {
            assert!(saved.get(field).is_none());
            assert!(saved["shards"][0].get(field).is_none());
        }
    }
}
#[test]
fn pass_through_replication_and_recovery_preserve_literal_bytes() {
    for bytes in FIXTURES {
        let (b, w) = setup();
        put(&b, "source", bytes);
        let loaded =
            load_manifest_bytes_with_storage(w.reader(), "src:golden/manifest.json").unwrap();
        assert_eq!(loaded, bytes);
        replicate_manifest_bytes_with_storage(&w, &loaded, &["dst:".into()], 1).unwrap();
        assert_eq!(
            b.read_all(
                &OperationContext::none(),
                &ObjectKey::new("replica").unwrap(),
                None
            )
            .unwrap(),
            bytes
        );
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("recovered.json");
        std::fs::write(&output, b"previous").unwrap();
        recover_manifest_with_storage(w.reader(), "golden", &["src:".into()], &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert_eq!(
            b.read_all(
                &OperationContext::none(),
                &ObjectKey::new("source").unwrap(),
                None
            )
            .unwrap(),
            bytes
        );
    }
}
#[test]
fn invalid_manifest_never_replaces_existing_destination() {
    let (b, w) = setup();
    put(&b, "source", b"{}");
    put(&b, "replica", b"old");
    assert!(replicate_manifest_bytes_with_storage(&w, b"{}", &["dst:".into()], 1).is_err());
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    std::fs::write(&out, b"old").unwrap();
    assert!(recover_manifest_with_storage(w.reader(), "golden", &["src:".into()], &out).is_err());
    assert_eq!(std::fs::read(out).unwrap(), b"old");
    assert_eq!(
        b.read_all(
            &OperationContext::none(),
            &ObjectKey::new("replica").unwrap(),
            None
        )
        .unwrap(),
        b"old"
    );
}
#[test]
fn content_root_domains_and_exact_hash_bytes_are_stable() {
    for bytes in FIXTURES {
        let mut m: Manifest = serde_json::from_slice(bytes).unwrap();
        m.archive_id = "other".into();
        m.original_name = "other".into();
        m.created_unix = 999;
        validate_manifest(&m).unwrap();
        m.shards[0].object.push('/');
        if m.version == 1 {
            validate_manifest(&m).unwrap();
        } else {
            assert!(validate_manifest(&m).is_err());
            m.shards[0].object.pop();
        }
        m.shards[0].blake3 = m.shards[0].blake3.to_uppercase();
        assert!(validate_manifest(&m).is_err());
    }
}
