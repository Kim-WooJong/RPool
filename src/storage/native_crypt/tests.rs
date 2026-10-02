use super::*;
use crate::crypt::obscure::obscure;
use crate::crypt::options::CryptConfig;
use crate::storage::error::StorageErrorKind;
use crate::storage::memory::faults::{Fault, FaultBackend, Operation, Rule};
use crate::storage::memory::MemoryBackend;
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn cipher(password: &str) -> Arc<Cipher> {
    // scrypt is deliberately slow; derive each test key once.
    static CACHE: OnceLock<std::sync::Mutex<BTreeMap<String, Arc<Cipher>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    cache
        .entry(password.to_string())
        .or_insert_with(|| {
            let section: BTreeMap<String, String> = [
                ("type", "crypt".to_string()),
                ("remote", "base:".to_string()),
                ("password", obscure(password).unwrap()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            Arc::new(Cipher::new(&CryptConfig::from_section(&section).unwrap()).unwrap())
        })
        .clone()
}

fn setup(inner: Arc<dyn StorageBackend>) -> CryptBackend {
    CryptBackend::new(
        BackendId::new("crypt").unwrap(),
        inner,
        cipher("pool secret"),
    )
}

fn memory() -> Arc<MemoryBackend> {
    Arc::new(MemoryBackend::new(BackendId::new("base").unwrap()))
}

fn key(raw: &str) -> ObjectKey {
    ObjectKey::new(raw).unwrap()
}

fn sample(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 131 % 251) as u8).collect()
}

fn ctx() -> OperationContext {
    OperationContext::none()
}

fn put(backend: &dyn StorageBackend, name: &str, bytes: &[u8]) -> WriteReceipt {
    backend
        .write(
            &ctx(),
            &key(name),
            &mut &bytes[..],
            &WriteOptions::default(),
        )
        .unwrap()
}

fn read(
    backend: &dyn StorageBackend,
    name: &str,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, StorageError> {
    let mut out = Vec::new();
    let receipt = backend.read(
        &ctx(),
        &key(name),
        &ReadRange::new(offset, length)?,
        &mut out,
    )?;
    assert_eq!(receipt.bytes_read, out.len() as u64);
    Ok(out)
}

#[test]
fn inner_backend_only_sees_rclone_crypt_names_and_bytes() {
    let base = memory();
    let crypt = setup(base.clone());
    let plain = sample(3 * 65536 + 99);
    let receipt = put(&crypt, "archive/shard-0001.rpool", &plain);
    assert_eq!(receipt.size, plain.len() as u64);

    assert_eq!(
        base.stat(&ctx(), &key("archive/shard-0001.rpool"))
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );
    let stored_key = crypt.file_key(&key("archive/shard-0001.rpool")).unwrap();
    assert!(!stored_key.as_str().contains("archive"));
    let stored = base.read_all(&ctx(), &stored_key, None).unwrap();
    assert_eq!(
        stored.len() as u64,
        data::encrypted_size(plain.len() as u64)
    );
    // The same bytes decrypt with the plain library: this is the rclone format.
    assert_eq!(cipher("pool secret").decrypt_bytes(&stored).unwrap(), plain);
    assert_eq!(
        crypt
            .stat(&ctx(), &key("archive/shard-0001.rpool"))
            .unwrap()
            .size,
        plain.len() as u64
    );
}

#[test]
fn ranged_reads_match_plaintext_windows() {
    let crypt = setup(memory());
    for len in [0usize, 1, 65535, 65536, 65537, 2 * 65536 + 7] {
        let plain = sample(len);
        put(&crypt, "obj", &plain);
        assert_eq!(crypt.read_all(&ctx(), &key("obj"), None).unwrap(), plain);
        assert_eq!(
            crypt.read_all(&ctx(), &key("obj"), Some(10)).unwrap(),
            plain[..len.min(10)]
        );
        for offset in [0u64, 1, 65535, 65536, 65537, len as u64, len as u64 + 10] {
            for length in [0u64, 1, 100, 65536, 200_000] {
                let got = read(&crypt, "obj", offset, length).unwrap();
                let start = (offset as usize).min(len);
                let end = (offset as usize).saturating_add(length as usize).min(len);
                assert_eq!(
                    got,
                    plain[start..end],
                    "len {len} offset {offset} length {length}"
                );
            }
        }
    }
}

#[test]
fn corrupted_truncated_and_foreign_objects_are_corrupt_data() {
    let base = memory();
    let crypt = setup(base.clone());
    let plain = sample(70_000);
    put(&crypt, "obj", &plain);
    let stored_key = crypt.file_key(&key("obj")).unwrap();
    let stored = base.read_all(&ctx(), &stored_key, None).unwrap();
    let replace = |bytes: &[u8]| {
        base.write(
            &ctx(),
            &stored_key,
            &mut &bytes[..],
            &WriteOptions::default(),
        )
        .unwrap();
    };
    let mut flipped = stored.clone();
    flipped[40] ^= 0x80;
    replace(&flipped);
    assert_eq!(
        crypt
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::CorruptData
    );
    // An untouched earlier window still reads; the damaged block does not.
    let mut late = stored.clone();
    let last = late.len() - 1;
    late[last] ^= 1;
    replace(&late);
    assert_eq!(read(&crypt, "obj", 0, 1000).unwrap(), plain[..1000]);
    assert_eq!(
        read(&crypt, "obj", 69_000, 1000).unwrap_err().kind(),
        StorageErrorKind::CorruptData
    );
    replace(&stored[..stored.len() - 5]);
    assert_eq!(
        crypt
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::CorruptData
    );
    replace(&stored[..HEADER_SIZE as usize + 10]);
    assert_eq!(
        crypt.stat(&ctx(), &key("obj")).unwrap_err().kind(),
        StorageErrorKind::CorruptData
    );
    replace(b"plain text, not crypt");
    assert_eq!(
        crypt
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::CorruptData
    );
    // Another password maps the key elsewhere and cannot open these bytes.
    replace(&stored);
    let other = CryptBackend::new(
        BackendId::new("other").unwrap(),
        base.clone(),
        cipher("other"),
    );
    let other_key = other.file_key(&key("obj")).unwrap();
    base.write(
        &ctx(),
        &other_key,
        &mut &stored[..],
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(
        other
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::CorruptData
    );
}

#[test]
fn inner_faults_propagate_with_their_meaning() {
    let base = memory();
    setup(base.clone())
        .write(
            &ctx(),
            &key("obj"),
            &mut &sample(70_000)[..],
            &WriteOptions::default(),
        )
        .unwrap();
    let with = |rules: Vec<Rule>| setup(Arc::new(FaultBackend::new(base.clone(), rules).unwrap()));
    let rule = |operation, call, fault| Rule {
        operation,
        call,
        fault,
    };

    // Header read (call 1) and body read (call 2) corruption.
    for call in [1, 2] {
        let crypt = with(vec![rule(Operation::Read, call, Fault::CorruptRead)]);
        assert_eq!(
            crypt
                .read_all(&ctx(), &key("obj"), None)
                .unwrap_err()
                .kind(),
            StorageErrorKind::CorruptData
        );
    }
    let crypt = with(vec![rule(Operation::Read, 2, Fault::ShortRead(70_000))]);
    assert_eq!(
        crypt
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::CorruptData
    );
    let crypt = with(vec![rule(
        Operation::Read,
        2,
        Fault::Error(StorageError::TransientIo {
            detail: "net".into(),
        }),
    )]);
    assert_eq!(
        crypt
            .read_all(&ctx(), &key("obj"), None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::TransientIo
    );

    let crypt = with(vec![rule(Operation::Write, 1, Fault::LoseResponse(None))]);
    let error = crypt
        .write(
            &ctx(),
            &key("new"),
            &mut &sample(10)[..],
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::UnknownOutcome);
    assert_eq!(
        setup(base.clone())
            .read_all(&ctx(), &key("new"), None)
            .unwrap(),
        sample(10)
    );

    let crypt = with(vec![rule(Operation::Write, 1, Fault::PartialWrite(1000))]);
    assert!(crypt
        .write(
            &ctx(),
            &key("partial"),
            &mut &sample(200_000)[..],
            &WriteOptions::default()
        )
        .is_err());
    assert_eq!(
        setup(base.clone())
            .stat(&ctx(), &key("partial"))
            .unwrap_err()
            .kind(),
        StorageErrorKind::NotFound
    );

    // Conditional create is enforced on the encrypted key.
    let crypt = setup(base.clone());
    let create = WriteOptions {
        overwrite: false,
        expected_version: None,
        defer_hash_check: false,
    };
    assert_eq!(
        crypt
            .write(&ctx(), &key("obj"), &mut &b"x"[..], &create)
            .unwrap_err()
            .kind(),
        StorageErrorKind::AlreadyExists
    );
    crypt.delete(&ctx(), &key("obj")).unwrap();
    assert_eq!(
        crypt.stat(&ctx(), &key("obj")).unwrap_err().kind(),
        StorageErrorKind::NotFound
    );
}

#[test]
fn source_errors_fail_the_write_without_publishing() {
    struct Failing(usize);
    impl Read for Failing {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("source broke"));
            }
            let n = out.len().min(self.0);
            out[..n].fill(7);
            self.0 -= n;
            Ok(n)
        }
    }
    let crypt = setup(memory());
    assert!(crypt
        .write(
            &ctx(),
            &key("obj"),
            &mut Failing(100_000),
            &WriteOptions::default()
        )
        .is_err());
    assert_eq!(
        crypt.stat(&ctx(), &key("obj")).unwrap_err().kind(),
        StorageErrorKind::NotFound
    );
}

#[test]
fn keys_that_cannot_be_stored_are_invalid_input() {
    let crypt = setup(memory());
    let long = "x".repeat(3000);
    let error = crypt
        .write(&ctx(), &key(&long), &mut &b""[..], &WriteOptions::default())
        .unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidInput);
    assert_eq!(
        crypt
            .list(&ctx(), "partial-prefix", None)
            .unwrap_err()
            .kind(),
        StorageErrorKind::Unsupported
    );
}

/// Interop with the installed rclone: RPool-written objects read back through a
/// real rclone crypt remote and vice versa, using a plain local directory as the
/// base remote. `RPOOL_TEST_RCLONE` overrides the binary.
#[test]
#[ignore = "requires rclone"]
fn objects_are_interchangeable_with_an_rclone_crypt_remote() {
    let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone".into()
        } else {
            "rclone".into()
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let base_dir = temp.path().join("base");
    std::fs::create_dir(&base_dir).unwrap();
    let conf = temp.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[c]\ntype = crypt\nremote = {}\npassword = {}\n",
            base_dir.display(),
            obscure("pool secret").unwrap()
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new(&rclone)
            .args(args)
            .arg("--config")
            .arg(&conf)
            .env_clear()
            .env("HOME", temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };

    // RPool → rclone: export the inner objects as files under their stored keys.
    let base = memory();
    let crypt = setup(base.clone());
    let objects = [
        ("pool/a/shard-1", sample(200_001)),
        ("pool/b/empty", Vec::new()),
    ];
    for (name, bytes) in &objects {
        put(&crypt, name, bytes);
        let stored_key = crypt.file_key(&key(name)).unwrap();
        let path = base_dir.join(stored_key.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, base.read_all(&ctx(), &stored_key, None).unwrap()).unwrap();
        assert_eq!(&run(&["cat", &format!("c:{name}")]), bytes, "{name}");
    }

    // rclone → RPool: import the files rclone wrote into a fresh inner store.
    let source = temp.path().join("plain");
    let from_rclone = sample(3 * 65536 + 5);
    std::fs::write(&source, &from_rclone).unwrap();
    run(&["copyto", source.to_str().unwrap(), "c:from/rclone.bin"]);
    let imported = memory();
    let expected_key = crypt.file_key(&key("from/rclone.bin")).unwrap();
    let bytes = std::fs::read(base_dir.join(expected_key.as_str())).unwrap();
    imported
        .write(
            &ctx(),
            &expected_key,
            &mut &bytes[..],
            &WriteOptions::default(),
        )
        .unwrap();
    let reader = setup(imported);
    assert_eq!(
        reader
            .read_all(&ctx(), &key("from/rclone.bin"), None)
            .unwrap(),
        from_rclone
    );
    assert_eq!(
        read(&reader, "from/rclone.bin", 65530, 70_000).unwrap(),
        from_rclone[65530..135_530]
    );
    assert_eq!(
        reader.stat(&ctx(), &key("from/rclone.bin")).unwrap().size,
        from_rclone.len() as u64
    );
}
