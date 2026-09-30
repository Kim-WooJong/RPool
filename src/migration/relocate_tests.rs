use super::*;
use crate::planning::build_upload_plan;
use crate::storage::memory::MemoryBackend;
use crate::storage::reader::StorageReader;
use crate::storage::reference::{BackendId, ObjectKey, ObjectRef};
use crate::storage::registry::BackendRegistry;
use crate::storage::traits::{OperationContext, StorageBackend, WriteOptions};

const MIB: u64 = 1024 * 1024;
const NEW: &str = "relocated-new";
const ALL_REMOTES: [&str; 5] = ["a:", "b:", "c:", "d:", "e:"];

struct Fixture {
    memory: Arc<MemoryBackend>,
    bindings: BTreeMap<String, ObjectRef>,
    temp: tempfile::TempDir,
    bytes: Vec<u8>,
    manifest: Manifest,
}

impl Fixture {
    /// RS 2+1, 1 MiB shards, `size` bytes spread round-robin over `remotes`.
    fn new(remotes: &[&str], size: usize) -> Self {
        Self::with_parity(remotes, size, 1)
    }
    fn with_parity(remotes: &[&str], size: usize, parity: usize) -> Self {
        let coding = Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 2,
            parity_shards: parity,
            stripe_size: 64 * 1024,
        };
        let plan = build_upload_plan(
            "no-rclone",
            size as u64,
            MIB,
            "old".into(),
            remotes.iter().map(|r| (*r).to_owned()).collect(),
            Placement::RoundRobin,
            Some(coding.clone()),
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..size)
            .map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1])
            .collect();
        let path = temp.path().join("source");
        fs::write(&path, &bytes).unwrap();
        let mut fixture = Self {
            memory: Arc::new(MemoryBackend::new(BackendId::new("relocate").unwrap())),
            bindings: BTreeMap::new(),
            temp,
            bytes,
            manifest: Manifest {
                version: 2,
                archive_id: "old".into(),
                original_name: "source".into(),
                original_size: size as u64,
                shard_size: MIB,
                created_unix: 7,
                content_root_blake3: String::new(),
                coding: Some(coding.clone()),
                shards: vec![],
            },
        };
        for shard in &plan.shards {
            fixture.bind(&shard.object);
        }
        let writer = fixture.writer();
        let mut shards: Vec<Shard> = plan
            .shards
            .iter()
            .filter(|p| p.kind == ShardKind::Data)
            .map(|p| crate::storage::upload_one_data_shard(&writer, &path, p, 1).unwrap())
            .collect();
        let groups = crate::manifest::coding_group_count(shards.len(), 2);
        for group in 0..groups {
            for item in crate::erasure::generate_parity_group(
                &path,
                &plan,
                &coding,
                group as u32,
                fixture.temp.path(),
            )
            .unwrap()
            {
                let shard = crate::planning::shard_from_plan(&item.plan, item.blake3);
                writer.write_file(&item.path, 0, &shard, 1).unwrap();
                shards.push(shard);
            }
        }
        shards.sort_by_key(|s| s.index);
        fixture.manifest.content_root_blake3 =
            content_root_v2(size as u64, MIB, &Some(coding), &shards);
        fixture.manifest.shards = shards;
        validate_manifest(&fixture.manifest).unwrap();
        // Every possible destination of the new archive.
        for shard in fixture.manifest.shards.clone() {
            for remote in ALL_REMOTES {
                fixture.bind(&remote_join(
                    remote,
                    &relative_object(NEW, &shard, fixture.manifest.coding.as_ref()),
                ));
            }
        }
        for remote in ALL_REMOTES {
            fixture.bind(&remote_join(remote, &format!("{NEW}/manifest.json")));
        }
        fixture
    }
    fn bind(&mut self, raw: &str) {
        if !self.bindings.contains_key(raw) {
            let key = ObjectKey::new(format!("object-{}", self.bindings.len())).unwrap();
            self.bindings
                .insert(raw.into(), ObjectRef::new(self.memory.id(), key));
        }
    }
    /// Storage where the given remotes are gone from the rclone config.
    fn writer_without(&self, gone: &[&str]) -> StorageWriter {
        let bindings = self
            .bindings
            .iter()
            .filter(|(raw, _)| !gone.iter().any(|remote| raw.starts_with(remote)))
            .map(|(raw, reference)| (raw.clone(), reference.clone()))
            .collect();
        let mut registry = BackendRegistry::new();
        registry.register(self.memory.clone()).unwrap();
        StorageWriter::synthetic(StorageReader::from_registry(
            registry,
            bindings,
            OperationContext::none(),
        ))
    }
    fn writer(&self) -> StorageWriter {
        self.writer_without(&[])
    }
    fn read(&self, raw: &str) -> Option<Vec<u8>> {
        self.memory
            .read_all(&OperationContext::none(), self.bindings[raw].key(), None)
            .ok()
    }
    fn delete(&self, raw: &str) {
        self.memory
            .delete(&OperationContext::none(), self.bindings[raw].key())
            .unwrap();
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
    fn old_objects(&self) -> BTreeMap<String, Option<Vec<u8>>> {
        self.manifest
            .shards
            .iter()
            .map(|s| (s.object.clone(), self.read(&s.object)))
            .collect()
    }
    fn new_objects_written(&self) -> usize {
        self.bindings
            .keys()
            .filter(|raw| raw.contains(&format!("{NEW}/")))
            .filter(|raw| self.read(raw).is_some())
            .count()
    }
    fn restore(&self, writer: &StorageWriter, manifest: &Manifest) -> Vec<u8> {
        let path = self.temp.path().join("new-manifest.json");
        fs::write(&path, serde_json::to_vec(manifest).unwrap()).unwrap();
        let output = self.temp.path().join("restored");
        crate::commands::get_with_storage(writer.reader(), path.to_str().unwrap(), &output, 2, 1)
            .unwrap();
        fs::read(output).unwrap()
    }
}

fn target(remotes: &[&str], placement: Placement) -> PoolDefinition {
    PoolDefinition {
        remotes: remotes.iter().map(|r| (*r).to_owned()).collect(),
        shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
        workers: 2,
        retries: 1,
        placement,
        data_shards: 2,
        parity_shards: 1,
        max_object_bytes: None,
        native_crypt: false,
    }
}

fn names(remotes: &[&str]) -> Vec<String> {
    remotes.iter().map(|r| (*r).to_owned()).collect()
}

fn run(
    fixture: &Fixture,
    writer: &StorageWriter,
    pool: &PoolDefinition,
    domains: &[&str],
) -> Result<Relocated> {
    run_with(fixture, writer, None, pool, domains)
}

fn run_with(
    fixture: &Fixture,
    writer: &StorageWriter,
    copier: Option<&dyn ShardCopier>,
    pool: &PoolDefinition,
    domains: &[&str],
) -> Result<Relocated> {
    let work = fixture.temp.path().join("work");
    relocate_with_storage(
        writer,
        copier,
        &fixture.manifest,
        pool,
        &pool.remotes,
        &names(domains),
        NEW,
        &work,
    )
}

fn assert_outage_bound(manifest: &Manifest, domain_of: impl Fn(&str) -> String) {
    let mut counts = BTreeMap::<(u32, String), usize>::new();
    for shard in &manifest.shards {
        *counts
            .entry((shard.group, domain_of(&shard.remote)))
            .or_default() += 1;
    }
    assert!(counts.values().all(|count| *count <= 1), "{counts:?}");
}

const SIZE: usize = 3 * MIB as usize + 12_345; // 4 data shards, 2 groups, 6 shards

#[test]
fn removed_remote_is_reconstructed_onto_new_remote_and_old_archive_is_untouched() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let before = fixture.old_objects();
    let writer = fixture.writer_without(&["c:"]);
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap();
    let manifest = &relocated.manifest;
    assert_eq!(manifest.archive_id, NEW);
    validate_manifest(manifest).unwrap();
    for (old, new) in fixture.manifest.shards.iter().zip(&manifest.shards) {
        assert_eq!(
            (old.blake3.as_str(), old.size),
            (new.blake3.as_str(), new.size)
        );
        assert!(new.object.starts_with(&format!("{}{NEW}/", new.remote)));
        assert_ne!(new.remote, "c:");
        if old.remote != "c:" {
            assert_eq!(old.remote, new.remote, "healthy shard changed remote");
        }
    }
    assert_outage_bound(manifest, |remote| remote.to_owned());
    // Only readable shards are downloaded; the c: shards are rebuilt.
    let on_c: u64 = fixture
        .manifest
        .shards
        .iter()
        .filter(|s| s.remote == "c:")
        .map(|s| s.size)
        .sum();
    let total: u64 = fixture.manifest.shards.iter().map(|s| s.size).sum();
    assert!(on_c > 0);
    assert_eq!(relocated.downloaded_bytes, total - on_c);
    assert_eq!(relocated.uploaded_bytes, total);
    assert_eq!(fixture.restore(&writer, manifest), fixture.bytes);
    assert_eq!(fixture.old_objects(), before);
    // Replicas are on the target remotes and hold the returned manifest.
    assert_eq!(relocated.manifest_locations.len(), 3);
    for location in &relocated.manifest_locations {
        let bytes = fixture.read(location).unwrap();
        let replica: Manifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            crate::manifest::manifest_fingerprint(&replica).unwrap(),
            crate::manifest::manifest_fingerprint(manifest).unwrap()
        );
    }
    assert!(!fs::read_dir(fixture.temp.path().join("work"))
        .unwrap()
        .any(|_| true));
}

#[test]
fn removed_but_readable_remote_is_copied_verbatim() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let before = fixture.old_objects();
    let writer = fixture.writer();
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap();
    let total: u64 = fixture.manifest.shards.iter().map(|s| s.size).sum();
    // Every shard is fetched exactly once; nothing needed Reed-Solomon.
    assert_eq!(relocated.downloaded_bytes, total);
    assert_eq!(relocated.uploaded_bytes, total);
    for (old, new) in fixture
        .manifest
        .shards
        .iter()
        .zip(&relocated.manifest.shards)
    {
        if old.remote == "c:" {
            assert_eq!(new.remote, "d:");
        } else {
            assert_eq!(old.remote, new.remote);
        }
    }
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
    assert_eq!(fixture.old_objects(), before);
}

#[test]
fn resilient_relocation_respects_outage_groups() {
    // a: and b: share outage group x; the source put one shard of each group on each.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer_without(&["c:"]);
    let pool = target(&["a:", "b:", "d:", "e:"], Placement::Resilient);
    let domain = |remote: &str| match remote {
        "a:" | "b:" => "x".to_owned(),
        "d:" => "y".to_owned(),
        _ => "z".to_owned(),
    };
    let relocated = run(&fixture, &writer, &pool, &["x", "x", "y", "z"]).unwrap();
    assert_outage_bound(&relocated.manifest, domain);
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
    // Too few independent outage groups: refused before anything is written.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer_without(&["c:"]);
    let pool = target(&["a:", "b:", "d:"], Placement::Resilient);
    assert!(run(&fixture, &writer, &pool, &["x", "x", "y"]).is_err());
    assert_eq!(fixture.new_objects_written(), 0);
}

#[test]
fn unrecoverable_group_is_a_typed_error_and_writes_nothing() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    // Group 0 loses its c: shard (remote removed) and another one (missing).
    let victim = fixture
        .manifest
        .shards
        .iter()
        .find(|s| s.group == 0 && s.remote != "c:")
        .unwrap()
        .clone();
    fixture.delete(&victim.object);
    let before = fixture.old_objects();
    let writer = fixture.writer_without(&["c:"]);
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let error = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap_err();
    let typed = error.downcast_ref::<Unrecoverable>().unwrap();
    assert!(format!("{error}").starts_with("LostGroup"));
    assert_eq!(typed.0.len(), 1);
    let loss = &typed.0[0];
    assert_eq!((loss.group, loss.required_k, loss.available), (0, 2, 1));
    let reasons: BTreeSet<_> = loss
        .missing
        .iter()
        .map(|m| format!("{:?}", m.reason))
        .collect();
    assert_eq!(
        reasons,
        BTreeSet::from(["Missing".to_owned(), "RemoteRemoved".to_owned()])
    );
    assert_eq!(fixture.new_objects_written(), 0);
    assert_eq!(fixture.old_objects(), before);
}

#[test]
fn provider_error_on_pool_remote_is_unknown_not_lost() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let victim = fixture
        .manifest
        .shards
        .iter()
        .find(|s| s.group == 0 && s.remote != "c:")
        .unwrap()
        .clone();
    // c: is removed; the victim's route is unavailable (an error, not a loss).
    let mut registry = BackendRegistry::new();
    registry.register(fixture.memory.clone()).unwrap();
    let bindings = fixture
        .bindings
        .iter()
        .filter(|(raw, _)| !raw.starts_with("c:") && **raw != victim.object)
        .map(|(raw, reference)| (raw.clone(), reference.clone()))
        .collect();
    let writer = StorageWriter::synthetic(StorageReader::from_registry(
        registry,
        bindings,
        OperationContext::none(),
    ));
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let error = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap_err();
    assert!(error.downcast_ref::<Unrecoverable>().is_none());
    let typed = error.downcast_ref::<RelocateError>().unwrap();
    assert!(typed.orphans.is_empty());
    assert!(format!("{error}").starts_with("UnknownGroup"));
    assert_eq!(fixture.new_objects_written(), 0);
}

#[test]
fn retry_with_same_id_reuses_verified_objects_and_never_overwrites_foreign_data() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer_without(&["c:"]);
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let first = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap();
    let second = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap();
    assert_eq!(second.uploaded_bytes, 0);
    assert_eq!(second.downloaded_bytes, 0);
    assert_eq!(
        crate::manifest::manifest_fingerprint(&first.manifest).unwrap(),
        crate::manifest::manifest_fingerprint(&second.manifest).unwrap()
    );

    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer_without(&["c:"]);
    let first_destination = remote_join(
        &fixture.manifest.shards[0].remote,
        &relative_object(
            NEW,
            &fixture.manifest.shards[0],
            fixture.manifest.coding.as_ref(),
        ),
    );
    fixture.seed(&first_destination, b"someone else's object");
    assert!(run(&fixture, &writer, &pool, &["a", "b", "d"]).is_err());
    assert_eq!(
        fixture.read(&first_destination).unwrap(),
        b"someone else's object"
    );
    assert_eq!(fixture.new_objects_written(), 1);
}

#[test]
fn inputs_must_keep_coding_and_use_a_fresh_id() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let mut pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    pool.parity_shards = 2;
    assert!(run(&fixture, &writer, &pool, &["a", "b", "d"]).is_err());
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let work = fixture.temp.path().join("work");
    for id in ["old", "", "../x", "a/b"] {
        assert!(relocate_with_storage(
            &writer,
            None,
            &fixture.manifest,
            &pool,
            &pool.remotes,
            &names(&["a", "b", "d"]),
            id,
            &work,
        )
        .is_err());
    }
    let mut limited = pool.clone();
    limited.max_object_bytes = Some(MIB);
    assert!(run(&fixture, &writer, &limited, &["a", "b", "d"]).is_err());
    assert_eq!(fixture.new_objects_written(), 0);
}

#[test]
fn writes_use_the_pool_writer_with_native_crypt() {
    let mut pool = target(&["a:"], Placement::RoundRobin);
    assert!(!pool_writer("rclone", &pool).is_native());
    pool.native_crypt = true;
    assert!(pool_writer("rclone", &pool).is_native());
}

#[test]
fn corruption_found_on_download_switches_to_reconstruction() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    // Same size, wrong content: the quick probe passes, the verified read fails.
    let victim = fixture
        .manifest
        .shards
        .iter()
        .find(|s| s.remote == "a:" && s.kind == ShardKind::Data)
        .unwrap()
        .clone();
    fixture.seed(&victim.object, &vec![0xAA; victim.size as usize]);
    let writer = fixture.writer();
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run(&fixture, &writer, &pool, &["a", "b", "d"]).unwrap();
    let rebuilt = &relocated.manifest.shards[victim.index as usize];
    assert_eq!(rebuilt.blake3, victim.blake3);
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
    // The corrupt source object itself is left as it was.
    assert_eq!(
        fixture.read(&victim.object).unwrap(),
        vec![0xAA; victim.size as usize]
    );
}

/// Copier over the fixture's memory objects. The memory backend stores the
/// plaintext, so the "ciphertext hash" is the object's own hash.
struct FakeCopier<'a> {
    fixture: &'a Fixture,
    /// Remotes gone from the rclone config.
    gone: Vec<&'static str>,
    server_side: bool,
    hashes: bool,
    /// Flip a byte of every copy (a copy that does not match its source).
    corrupt: bool,
    /// Report every destination as existing (a racing writer).
    exists: bool,
    copies: Mutex<Vec<(String, String)>>,
}

impl<'a> FakeCopier<'a> {
    fn new(fixture: &'a Fixture, gone: &[&'static str]) -> Self {
        Self {
            fixture,
            gone: gone.to_vec(),
            server_side: true,
            hashes: true,
            corrupt: false,
            exists: false,
            copies: Mutex::new(Vec::new()),
        }
    }
    fn readable(&self, address: &str) -> Result<Vec<u8>> {
        if self.gone.iter().any(|r| address.starts_with(r)) {
            return Err(StorageError::invalid_input("remote is not configured").into());
        }
        self.fixture
            .read(address)
            .ok_or_else(|| StorageError::not_found("object").into())
    }
    fn copies(&self) -> Vec<(String, String)> {
        self.copies.lock().unwrap().clone()
    }
}

impl ShardCopier for FakeCopier<'_> {
    fn copy(&self, source: &str, destination: &str) -> Result<()> {
        if self.exists || self.fixture.read(destination).is_some() {
            return Err(StorageError::AlreadyExists {
                path: "copy destination".into(),
            }
            .into());
        }
        let mut bytes = self.readable(source)?;
        if self.corrupt {
            bytes[0] ^= 0xff;
        }
        self.fixture.seed(destination, &bytes);
        self.copies
            .lock()
            .unwrap()
            .push((source.to_owned(), destination.to_owned()));
        Ok(())
    }
    fn server_side(&self, source: &str, destination: &str) -> bool {
        self.server_side && source.split(':').next() == destination.split(':').next()
    }
    fn stored_hash(&self, address: &str) -> Result<Option<(u64, String)>> {
        if !self.hashes {
            return Ok(None);
        }
        let bytes = self.readable(address)?;
        Ok(Some((
            bytes.len() as u64,
            blake3::hash(&bytes).to_hex().to_string(),
        )))
    }
}

fn bytes_on(manifest: &Manifest, pick: impl Fn(&Shard) -> bool) -> u64 {
    manifest
        .shards
        .iter()
        .filter(|s| pick(s))
        .map(|s| s.size)
        .sum()
}

#[test]
fn kept_shards_are_copied_server_side_and_hash_verified() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let before = fixture.old_objects();
    let writer = fixture.writer();
    let copier = FakeCopier::new(&fixture, &[]);
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    let total = bytes_on(&fixture.manifest, |_| true);
    let on_c = bytes_on(&fixture.manifest, |s| s.remote == "c:");
    let kept = fixture
        .manifest
        .shards
        .iter()
        .filter(|s| s.remote != "c:")
        .count();
    // Kept shards: provider copy, hash-verified. c: shards: streamed to d:.
    assert_eq!(relocated.server_side_bytes, total - on_c);
    assert_eq!(relocated.hash_verified_shards, kept);
    assert_eq!(
        (relocated.downloaded_bytes, relocated.uploaded_bytes),
        (on_c, on_c)
    );
    assert_eq!(
        relocated.readback_bytes, on_c,
        "only streamed copies are read back"
    );
    assert_eq!(copier.copies().len(), fixture.manifest.shards.len());
    for (old, new) in fixture
        .manifest
        .shards
        .iter()
        .zip(&relocated.manifest.shards)
    {
        let expected = if old.remote == "c:" {
            "d:"
        } else {
            old.remote.as_str()
        };
        assert_eq!(new.remote, expected);
        assert!(new.object.starts_with(&format!("{}{NEW}/", new.remote)));
    }
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
    assert_eq!(fixture.old_objects(), before);
}

#[test]
fn without_a_hash_server_side_copies_are_read_back() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let mut copier = FakeCopier::new(&fixture, &[]);
    copier.hashes = false;
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    let total = bytes_on(&fixture.manifest, |_| true);
    let on_c = bytes_on(&fixture.manifest, |s| s.remote == "c:");
    assert_eq!(relocated.hash_verified_shards, 0);
    assert_eq!(relocated.server_side_bytes, total - on_c);
    assert_eq!(relocated.readback_bytes, total);
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);

    // No server-side copy at all: every copy is streamed and read back.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let mut copier = FakeCopier::new(&fixture, &[]);
    copier.server_side = false;
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    assert_eq!(relocated.server_side_bytes, 0);
    assert_eq!(relocated.hash_verified_shards, 0);
    assert_eq!(
        (relocated.downloaded_bytes, relocated.uploaded_bytes),
        (total, total)
    );
    assert_eq!(relocated.readback_bytes, total);
}

#[test]
fn rebuild_groups_copy_kept_shards_and_upload_only_rebuilt_ones() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let before = fixture.old_objects();
    let writer = fixture.writer_without(&["c:"]);
    let copier = FakeCopier::new(&fixture, &["c:"]);
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    let total = bytes_on(&fixture.manifest, |_| true);
    let on_c = bytes_on(&fixture.manifest, |s| s.remote == "c:");
    // 2+1: the two kept shards of each group are needed for decoding.
    assert_eq!(relocated.downloaded_bytes, total - on_c);
    assert_eq!(
        relocated.uploaded_bytes, on_c,
        "only rebuilt shards are uploaded"
    );
    assert_eq!(relocated.server_side_bytes, total - on_c);
    assert_eq!(relocated.readback_bytes, on_c);
    assert!(copier
        .copies()
        .iter()
        .all(|(from, _)| !from.starts_with("c:")));
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
    assert_eq!(fixture.old_objects(), before);
}

#[test]
fn rebuild_downloads_only_k_shards_per_group() {
    // RS 2+2 over a:, b:, c:, d:; d: is gone, so every group has three
    // readable shards but only K = 2 are downloaded for decoding.
    let fixture = Fixture::with_parity(&["a:", "b:", "c:", "d:"], SIZE, 2);
    let writer = fixture.writer_without(&["d:"]);
    let copier = FakeCopier::new(&fixture, &["d:"]);
    let mut pool = target(&["a:", "b:", "c:", "e:"], Placement::RoundRobin);
    pool.parity_shards = 2;
    let relocated = run_with(
        &fixture,
        &writer,
        Some(&copier),
        &pool,
        &["a", "b", "c", "e"],
    )
    .unwrap();
    let groups = groups_of(&fixture.manifest);
    assert_eq!(groups.len(), 2);
    let on_d = bytes_on(&fixture.manifest, |s| s.remote == "d:");
    let readable = bytes_on(&fixture.manifest, |s| s.remote != "d:");
    // At most K (full-size) shards per group, less than every readable one.
    assert!(relocated.downloaded_bytes <= 2 * 2 * MIB);
    assert!(
        relocated.downloaded_bytes + MIB <= readable,
        "fetch stopped at K"
    );
    assert_eq!(relocated.uploaded_bytes, on_d);
    let total = bytes_on(&fixture.manifest, |_| true);
    assert_eq!(relocated.server_side_bytes, total - on_d);
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
}

#[test]
fn a_copy_that_does_not_match_is_replaced_by_a_verified_upload() {
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let mut copier = FakeCopier::new(&fixture, &[]);
    copier.corrupt = true;
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    assert_eq!(relocated.hash_verified_shards, 0);
    for shard in &relocated.manifest.shards {
        writer.reader().verify(shard, true).unwrap();
    }
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
}

#[test]
fn copies_never_overwrite_an_existing_destination() {
    // A destination that appears after the presence check (racing writer):
    // the copy refuses and relocation stops without an upload fallback.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let mut copier = FakeCopier::new(&fixture, &[]);
    copier.exists = true;
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let error = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap_err();
    assert!(is_already_exists(&error), "{error:#}");
    assert_eq!(fixture.new_objects_written(), 0);
    // Foreign data found by the presence check is never touched either.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let writer = fixture.writer();
    let copier = FakeCopier::new(&fixture, &[]);
    let first = remote_join(
        &fixture.manifest.shards[0].remote,
        &relative_object(
            NEW,
            &fixture.manifest.shards[0],
            fixture.manifest.coding.as_ref(),
        ),
    );
    fixture.seed(&first, b"foreign");
    assert!(run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).is_err());
    assert_eq!(fixture.read(&first).unwrap(), b"foreign");
    assert!(copier.copies().is_empty());
}

#[test]
fn unreadable_source_found_by_the_copy_is_rebuilt() {
    // The size probe passes but the source object is corrupt: the copy (a
    // streamed one, so it is read back) fails, the fallback download fails,
    // and the group is rebuilt.
    let fixture = Fixture::new(&["a:", "b:", "c:"], SIZE);
    let victim = fixture
        .manifest
        .shards
        .iter()
        .find(|s| s.remote == "a:" && s.kind == ShardKind::Data)
        .unwrap()
        .clone();
    fixture.seed(&victim.object, &vec![0xAA; victim.size as usize]);
    let writer = fixture.writer();
    let mut copier = FakeCopier::new(&fixture, &[]);
    copier.server_side = false;
    let pool = target(&["a:", "b:", "d:"], Placement::RoundRobin);
    let relocated = run_with(&fixture, &writer, Some(&copier), &pool, &["a", "b", "d"]).unwrap();
    assert_eq!(
        relocated.manifest.shards[victim.index as usize].blake3,
        victim.blake3
    );
    assert_eq!(fixture.restore(&writer, &relocated.manifest), fixture.bytes);
}

/// Docker e2e harness (scripts/linux-docker/migrate-relocate-e2e.sh): relocates
/// the manifest at RELOCATE_E2E_MANIFEST onto saved pool RELOCATE_E2E_POOL as
/// archive RELOCATE_E2E_ID with real rclone, writes the new manifest to
/// RELOCATE_E2E_OUTPUT and prints the transferred bytes.
#[test]
#[ignore = "needs rclone crypt remotes; run by scripts/linux-docker/migrate-relocate-e2e.sh"]
fn e2e_relocate_with_rclone() {
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let manifest = crate::manifest::load_manifest("rclone", &env("RELOCATE_E2E_MANIFEST")).unwrap();
    let store = crate::pool::load_pool_store().unwrap();
    let pool = store.pools[&env("RELOCATE_E2E_POOL")].clone();
    let work = tempfile::tempdir().unwrap();
    let relocated = relocate(
        "rclone",
        &manifest,
        &pool,
        &env("RELOCATE_E2E_ID"),
        work.path(),
    )
    .unwrap();
    fs::write(
        env("RELOCATE_E2E_OUTPUT"),
        serde_json::to_vec_pretty(&relocated.manifest).unwrap(),
    )
    .unwrap();
    println!(
        "RELOCATE_RESULT downloaded={} uploaded={} replicas={}",
        relocated.downloaded_bytes,
        relocated.uploaded_bytes,
        relocated.manifest_locations.join(",")
    );
}
