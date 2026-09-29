//! Peer-only immutable composition. Old objects are references, NEVER exclusive
//! ownership. No deletion/GC is permitted for these manifests. Equal-size layouts
//! only; resilient placement needs a separate retained-group topology proof.
use crate::prelude::*;
use crate::storage::writer::StorageWriter;

#[derive(Serialize, Deserialize)]
struct Group {
    number: u32,
    offset: u64,
    size: u64,
    hash: String,
    retained: Option<Vec<Shard>>,
}
#[derive(Serialize, Deserialize)]
struct Recipe {
    version: u32,
    id: String,
    source_hash: String,
    base_hash: String,
    policy_hash: String,
    created: u64,
    remotes: Vec<String>,
    groups: Vec<Group>,
}
#[derive(Serialize, Deserialize)]
struct Ready {
    recipe_hash: String,
    manifest: Manifest,
}
#[derive(Serialize, Deserialize)]
struct Checked<T> {
    hash: String,
    value: T,
}
fn save_checked<T: Serialize>(path: &Path, value: T) -> Result<()> {
    let hash = fingerprint(&value)?;
    super::namespace::durable_json(path, &Checked { hash, value })
}
fn load_checked<T: serde::de::DeserializeOwned + Serialize>(path: &Path) -> Result<T> {
    let stored: Checked<T> = read_json(path)?;
    if fingerprint(&stored.value)? != stored.hash {
        bail!("incremental checkpoint checksum mismatch");
    }
    Ok(stored.value)
}
fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir(path)?;
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("invalid incremental staging directory");
    }
    Ok(())
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        bail!("invalid incremental checkpoint");
    }
    crate::utils::read_json(path)
}
fn verify(storage: &StorageWriter, shards: &[Shard]) -> Result<()> {
    for shard in shards {
        storage.ensure_destination(&shard.object)?;
        storage.reader().verify(shard, true)?;
    }
    Ok(())
}

/// None is returned only before incremental remote writes. Caller must use this
/// only for append-only pool-sync, and must not register exclusive ownership.
pub(crate) fn upload(
    rclone: &str,
    policy: &PoolDefinition,
    pool: &str,
    source: &Path,
    id: &str,
    base: &Manifest,
) -> Result<Option<Manifest>> {
    let storage = StorageWriter::rclone(rclone);
    let status = super::capacity::CapacityStatus::inspect(
        &crate::storage::admin::RcloneAdmin::inherited(rclone),
        policy,
    )?;
    upload_with(
        policy,
        source,
        id,
        base,
        &status.eligible,
        |shards| verify(&storage, shards),
        |path, part_id| {
            super::workspace::upload_eligible_tracked(rclone, policy, pool, path, part_id)
                .map(|v| v.0)
        },
        |manifest, remotes| {
            crate::manifest::replicate_manifest_with_storage(
                &storage,
                manifest,
                remotes,
                policy.retries,
            )
            .map(|_| ())
        },
    )
}
fn upload_with(
    policy: &PoolDefinition,
    source: &Path,
    id: &str,
    base: &Manifest,
    eligible: &[String],
    mut verify_objects: impl FnMut(&[Shard]) -> Result<()>,
    mut upload_group: impl FnMut(&Path, &str) -> Result<Manifest>,
    mut publish: impl FnMut(&Manifest, &[String]) -> Result<()>,
) -> Result<Option<Manifest>> {
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        bail!("invalid incremental archive identity");
    }
    let root = source
        .parent()
        .context("incremental source parent missing")?
        .join(format!("peer-incremental-{id}"));
    let recipe_path = root.join("recipe.json");
    let resumed = recipe_path.exists();
    if root.exists() && !resumed {
        bail!("incremental staging lost its recipe; preserve staging");
    }
    crate::manifest::validate_manifest(base)?;
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("invalid incremental source");
    }
    let shard_size = policy.shard_bytes()?.get();
    let coding = (metadata.len() > 0 && policy.parity_shards > 0).then(|| Coding {
        algorithm: RS_ALGORITHM.into(),
        data_shards: policy.data_shards,
        parity_shards: policy.parity_shards,
        stripe_size: EC_STRIPE_SIZE,
    });
    let compatible = base.version == 2
        && metadata.len() > 0
        && base.original_size == metadata.len()
        && base.shard_size == shard_size
        && base.coding == coding
        && !matches!(
            policy.placement,
            Placement::Resilient | Placement::CapacityFirst
        );
    if !compatible {
        if resumed {
            bail!("incremental retry layout changed; preserve staging");
        }
        return Ok(None);
    }
    let source_hash = crate::utils::hash_file_range(source, 0, metadata.len())?;
    let base_hash = fingerprint(base)?;
    let policy_hash = fingerprint(policy)?;
    let recipe: Recipe = if resumed {
        directory(&root)?;
        let r: Recipe = load_checked(&recipe_path)?;
        if r.version != 1
            || r.id != id
            || r.source_hash != source_hash
            || r.base_hash != base_hash
            || r.policy_hash != policy_hash
            || r.remotes != eligible
        {
            bail!("incremental retry identity/configuration changed; preserve staging");
        }
        r
    } else {
        let data = crate::manifest::data_shards(base);
        let width = coding.as_ref().map_or(1, |c| c.data_shards);
        let mut groups = Vec::new();
        for (number, members) in data.chunks(width).enumerate() {
            let offset = members[0].offset;
            let size = members.iter().try_fold(0u64, |n, s| {
                n.checked_add(s.size).context("group size overflow")
            })?;
            let mut unchanged = true;
            for s in members {
                unchanged &= crate::utils::hash_file_range(source, s.offset, s.size)? == s.blake3;
            }
            let shards: Vec<_> = if coding.is_some() {
                base.shards
                    .iter()
                    .filter(|s| s.group == number as u32)
                    .cloned()
                    .collect()
            } else {
                vec![(*members[0]).clone()]
            };
            unchanged &= shards.iter().all(|s| eligible.contains(&s.remote));
            groups.push(Group {
                number: number as u32,
                offset,
                size,
                hash: crate::utils::hash_file_range(source, offset, size)?,
                retained: unchanged.then_some(shards),
            });
        }
        if !groups.iter().any(|g| g.retained.is_some()) {
            return Ok(None);
        }
        for g in &groups {
            if let Some(shards) = &g.retained {
                verify_objects(shards)?;
            }
        }
        let r = Recipe {
            version: 1,
            id: id.into(),
            source_hash: source_hash.clone(),
            base_hash,
            policy_hash,
            created: crate::utils::now_unix(),
            remotes: eligible.to_vec(),
            groups,
        };
        directory(&root)?;
        save_checked(&recipe_path, &r)?;
        r
    };
    let recipe_hash = fingerprint(&recipe)?;
    let ready_path = root.join("ready.json");
    let manifest = if ready_path.exists() {
        let ready: Ready = load_checked(&ready_path)?;
        if ready.recipe_hash != recipe_hash || ready.manifest.archive_id != id {
            bail!("incremental ready identity mismatch");
        }
        ready.manifest
    } else {
        let mut shards = Vec::new();
        for g in &recipe.groups {
            if let Some(retained) = &g.retained {
                verify_objects(retained)?;
                shards.extend(retained.clone());
                continue;
            }
            let dir = root.join(format!("g{}", g.number));
            directory(&dir)?;
            let path = dir.join("content.bin");
            if !path.exists() {
                let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
                let mut input = File::open(source)?;
                input.seek(SeekFrom::Start(g.offset))?;
                if std::io::copy(&mut input.take(g.size), &mut temp)? != g.size {
                    bail!("short incremental source");
                }
                temp.as_file().sync_all()?;
                temp.persist(&path).map_err(|e| e.error)?;
            }
            let m = fs::symlink_metadata(&path)?;
            if !m.is_file()
                || m.file_type().is_symlink()
                || m.len() != g.size
                || crate::utils::hash_file_range(&path, 0, g.size)? != g.hash
            {
                bail!("incremental group staging mismatch");
            }
            let part_id = format!("{id}-g{}", g.number);
            let part_path = dir.join("verified-part.json");
            let part: Manifest = if part_path.exists() {
                load_checked(&part_path)?
            } else {
                let part = upload_group(&path, &part_id)?;
                crate::manifest::validate_manifest(&part)?;
                verify_objects(&part.shards)?;
                save_checked(&part_path, &part)?;
                part
            };
            if part.archive_id != part_id
                || part.original_size != g.size
                || part
                    .shards
                    .iter()
                    .any(|s| !recipe.remotes.contains(&s.remote))
            {
                bail!("incremental subgroup identity/destination mismatch");
            }
            shards.extend(rebase(&part, base, g.number, g.offset)?);
        }
        shards.sort_by_key(|s| s.index);
        let manifest = Manifest {
            version: 2,
            archive_id: id.into(),
            original_name: source
                .file_name()
                .context("source filename missing")?
                .to_string_lossy()
                .into_owned(),
            original_size: metadata.len(),
            shard_size,
            created_unix: recipe.created,
            content_root_blake3: crate::manifest::content_root_v2(
                metadata.len(),
                shard_size,
                &coding,
                &shards,
            ),
            coding,
            shards,
        };
        crate::manifest::validate_manifest(&manifest)?;
        save_checked(
            &ready_path,
            Ready {
                recipe_hash,
                manifest: manifest.clone(),
            },
        )?;
        manifest
    };
    crate::manifest::validate_manifest(&manifest)?;
    if manifest.original_size != metadata.len()
        || manifest.shard_size != base.shard_size
        || manifest.coding != base.coding
    {
        bail!("incremental ready layout mismatch");
    }
    for shard in crate::manifest::data_shards(&manifest) {
        if crate::utils::hash_file_range(source, shard.offset, shard.size)? != shard.blake3 {
            bail!("incremental manifest does not describe source bytes");
        }
    }
    verify_objects(&manifest.shards)?;
    if crate::utils::hash_file_range(source, 0, metadata.len())? != source_hash {
        bail!("incremental source changed before publication");
    }
    publish(&manifest, &recipe.remotes)?;
    let reused: u64 = recipe
        .groups
        .iter()
        .filter(|g| g.retained.is_some())
        .map(|g| g.size)
        .sum();
    eprintln!("[peer incremental] reused logical bytes={reused}, changed logical bytes={}; verification reads and parity/metadata traffic are additional", metadata.len() - reused);
    Ok(Some(manifest))
}

fn rebase(part: &Manifest, base: &Manifest, group: u32, offset: u64) -> Result<Vec<Shard>> {
    crate::manifest::validate_manifest(part)?;
    if part.coding != base.coding || part.shard_size != base.shard_size {
        bail!("subgroup layout mismatch");
    }
    let count = crate::manifest::data_shards(base).len();
    let mut shards = part.shards.clone();
    for shard in &mut shards {
        if let Some(c) = &base.coding {
            shard.index = u32::try_from(if shard.kind == ShardKind::Data {
                group as usize * c.data_shards + shard.slot as usize
            } else {
                count + group as usize * c.parity_shards + shard.slot as usize - c.data_shards
            })?;
            shard.group = group;
        } else {
            shard.index = group;
        }
        if shard.kind == ShardKind::Data {
            shard.offset = shard
                .offset
                .checked_add(offset)
                .context("offset overflow")?;
        }
    }
    Ok(shards)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    type Objects = Rc<RefCell<BTreeMap<String, Vec<u8>>>>;
    fn stored(bytes: &[u8], id: &str, parity: bool, objects: &Objects) -> Manifest {
        let size = 1048576;
        let coding = parity.then(|| Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 2,
            parity_shards: 1,
            stripe_size: EC_STRIPE_SIZE,
        });
        let mut shards = vec![];
        for (n, chunk) in bytes.chunks(size).enumerate() {
            let object = format!("crypt:{id}/d{n}");
            objects.borrow_mut().insert(object.clone(), chunk.to_vec());
            shards.push(Shard {
                index: n as u32,
                offset: (n * size) as u64,
                size: chunk.len() as u64,
                remote: "crypt:".into(),
                object,
                blake3: blake3::hash(chunk).to_hex().to_string(),
                kind: ShardKind::Data,
                group: if parity { n as u32 / 2 } else { 0 },
                slot: if parity { (n % 2) as u16 } else { 0 },
            });
        }
        let count = shards.len();
        if parity {
            for g in 0..count.div_ceil(2) {
                let mut blocks = vec![vec![0u8; size]; 3];
                for (slot, block) in blocks.iter_mut().take(2).enumerate() {
                    let start = (g * 2 + slot) * size;
                    if start < bytes.len() {
                        let end = (start + size).min(bytes.len());
                        block[..end - start].copy_from_slice(&bytes[start..end]);
                    }
                }
                ReedSolomon::new(2, 1).unwrap().encode(&mut blocks).unwrap();
                let bytes = blocks.pop().unwrap();
                let object = format!("crypt:{id}/p{g}");
                objects.borrow_mut().insert(object.clone(), bytes.clone());
                shards.push(Shard {
                    index: (count + g) as u32,
                    offset: 0,
                    size: size as u64,
                    remote: "crypt:".into(),
                    object,
                    blake3: blake3::hash(&bytes).to_hex().to_string(),
                    kind: ShardKind::Parity,
                    group: g as u32,
                    slot: 2,
                });
            }
        }
        Manifest {
            version: 2,
            archive_id: id.into(),
            original_name: "file".into(),
            original_size: bytes.len() as u64,
            shard_size: size as u64,
            created_unix: 1,
            content_root_blake3: crate::manifest::content_root_v2(
                bytes.len() as u64,
                size as u64,
                &coding,
                &shards,
            ),
            coding,
            shards,
        }
    }
    fn check(objects: &Objects, shards: &[Shard]) -> Result<()> {
        for s in shards {
            let objects = objects.borrow();
            let bytes = objects.get(&s.object).context("missing object")?;
            if bytes.len() as u64 != s.size || blake3::hash(bytes).to_hex().as_str() != s.blake3 {
                bail!("bad object");
            }
        }
        Ok(())
    }
    fn policy(parity: bool) -> PoolDefinition {
        PoolDefinition {
            remotes: vec!["crypt:".into()],
            shard_size: crate::models::shard_size::ShardSize::from_mib(1).unwrap(),
            data_shards: 2,
            parity_shards: usize::from(parity),
            placement: Placement::RoundRobin,
            ..Default::default()
        }
    }
    #[test]
    fn real_flow_reuses_plain_shards_and_complete_rs_groups_without_borrowed_writes() {
        for parity in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("file");
            let mut bytes = vec![1; 4 * 1048576];
            let objects: Objects = Default::default();
            let base = stored(&bytes, "old", parity, &objects);
            let before = objects.borrow().clone();
            bytes[3 * 1048576] = 2;
            fs::write(&source, &bytes).unwrap();
            let mut calls = vec![];
            let result = upload_with(
                &policy(parity),
                &source,
                "new",
                &base,
                &["crypt:".into()],
                |s| check(&objects, s),
                |path, id| {
                    let b = fs::read(path)?;
                    calls.push(b.len());
                    Ok(stored(&b, id, parity, &objects))
                },
                |_, _| Ok(()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(calls, vec![if parity { 2 * 1048576 } else { 1048576 }]);
            for (k, v) in before {
                assert_eq!(objects.borrow().get(&k), Some(&v));
            }
            assert_eq!(result.shards[0].object, base.shards[0].object);
            let mut restored = vec![];
            for s in crate::manifest::data_shards(&result) {
                restored.extend_from_slice(&objects.borrow()[&s.object]);
            }
            assert_eq!(restored, bytes);
        }
    }
    #[test]
    fn failed_publication_resumes_identical_bytes_without_upload_and_rejects_changed_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        let mut bytes = vec![1; 2 * 1048576];
        let objects: Objects = Default::default();
        let base = stored(&bytes, "old", false, &objects);
        bytes[1048576] = 2;
        fs::write(&source, &bytes).unwrap();
        let policy = policy(false);
        let mut first = vec![];
        assert!(upload_with(
            &policy,
            &source,
            "new",
            &base,
            &policy.remotes,
            |s| check(&objects, s),
            |p, id| Ok(stored(&fs::read(p)?, id, false, &objects)),
            |m, _| {
                first = serde_json::to_vec(m)?;
                bail!("ambiguous publication")
            }
        )
        .is_err());
        let mut second = vec![];
        upload_with(
            &policy,
            &source,
            "new",
            &base,
            &policy.remotes,
            |s| check(&objects, s),
            |_, _| panic!("completed group must not upload again"),
            |m, _| {
                second = serde_json::to_vec(m)?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(first, second);
        let mut changed = policy.clone();
        changed.workers += 1;
        assert!(upload_with(
            &changed,
            &source,
            "new",
            &base,
            &policy.remotes,
            |_| Ok(()),
            |_, _| panic!(),
            |_, _| panic!()
        )
        .is_err());
        bytes[0] = 3;
        fs::write(&source, &bytes).unwrap();
        assert!(upload_with(
            &policy,
            &source,
            "new",
            &base,
            &policy.remotes,
            |_| Ok(()),
            |_, _| panic!(),
            |_, _| panic!()
        )
        .is_err());
    }
    #[test]
    fn unchanged_is_metadata_only_and_corrupt_retained_data_or_parity_fails_closed() {
        for corrupt in [None, Some(ShardKind::Data), Some(ShardKind::Parity)] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("file");
            let bytes = vec![1; 2 * 1048576];
            fs::write(&source, &bytes).unwrap();
            let objects: Objects = Default::default();
            let base = stored(&bytes, "old", true, &objects);
            if let Some(kind) = corrupt {
                let s = base.shards.iter().find(|s| s.kind == kind).unwrap();
                objects.borrow_mut().get_mut(&s.object).unwrap()[0] ^= 1;
            }
            let mut published = false;
            let result = upload_with(
                &policy(true),
                &source,
                "new",
                &base,
                &["crypt:".into()],
                |s| check(&objects, s),
                |_, _| panic!("unchanged groups must not upload"),
                |_, _| {
                    published = true;
                    Ok(())
                },
            );
            assert_eq!(result.is_ok(), corrupt.is_none());
            assert_eq!(published, corrupt.is_none());
        }
    }
    #[test]
    fn checksummed_recipe_detects_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        save_checked(&path, vec![1u32, 2]).unwrap();
        let mut value: Value = crate::utils::read_json(&path).unwrap();
        value["value"][0] = serde_json::json!(9);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load_checked::<Vec<u32>>(&path).is_err());
    }
    #[test]
    fn composed_manifest_recovers_missing_data_in_borrowed_and_new_rs_groups() {
        use crate::storage::{
            memory::MemoryBackend,
            reader::StorageReader,
            reference::{BackendId, ObjectKey, ObjectRef},
            registry::BackendRegistry,
            traits::{OperationContext, StorageBackend, WriteOptions},
        };
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        let mut bytes: Vec<u8> = (1..=4).flat_map(|v| vec![v; 1048576]).collect();
        let objects: Objects = Default::default();
        let base = stored(&bytes, "old", true, &objects);
        bytes[2 * 1048576 + 13] = 91;
        fs::write(&source, &bytes).unwrap();
        let composed = upload_with(
            &policy(true),
            &source,
            "new",
            &base,
            &["crypt:".into()],
            |s| check(&objects, s),
            |path, id| Ok(stored(&fs::read(path)?, id, true, &objects)),
            |_, _| Ok(()),
        )
        .unwrap()
        .unwrap();
        assert!(composed.shards[0].object.contains("old"));
        assert!(composed.shards[2].object.contains("new-g1"));
        let backend = Arc::new(MemoryBackend::new(
            BackendId::new("incremental-recovery").unwrap(),
        ));
        let mut bindings = BTreeMap::new();
        let ctx = OperationContext::none();
        for shard in &composed.shards {
            let key = ObjectKey::new(format!("shard-{}", shard.index)).unwrap();
            backend
                .write(
                    &ctx,
                    &key,
                    &mut std::io::Cursor::new(&objects.borrow()[&shard.object]),
                    &WriteOptions::default(),
                )
                .unwrap();
            bindings.insert(shard.object.clone(), ObjectRef::new(backend.id(), key));
        }
        for index in [0, 2] {
            backend
                .delete(&ctx, &ObjectKey::new(format!("shard-{index}")).unwrap())
                .unwrap();
        }
        let mut registry = BackendRegistry::new();
        registry.register(backend).unwrap();
        let reader = StorageReader::from_registry(registry, bindings, ctx);
        let cache =
            super::super::shard_cache::ShardCache::new(temp.path().join("cache"), 16 * 1048576)
                .unwrap();
        let recovered = cache
            .read(&reader, &composed, 0, bytes.len(), 2, 1)
            .unwrap();
        assert_eq!(recovered, bytes);
    }
    #[test]
    fn no_reuse_falls_back_without_staging_but_missing_recipe_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        let objects: Objects = Default::default();
        let base = stored(&vec![1; 1048576], "old", false, &objects);
        fs::write(&source, vec![2; 1048576]).unwrap();
        assert!(upload_with(
            &policy(false),
            &source,
            "new",
            &base,
            &["crypt:".into()],
            |_| panic!(),
            |_, _| panic!(),
            |_, _| panic!()
        )
        .unwrap()
        .is_none());
        let staging = temp.path().join("peer-incremental-new");
        assert!(!staging.exists());
        fs::create_dir(&staging).unwrap();
        assert!(upload_with(
            &policy(false),
            &source,
            "new",
            &base,
            &["crypt:".into()],
            |_| panic!(),
            |_, _| panic!(),
            |_, _| panic!()
        )
        .is_err());
    }
    fn manifest(data_count: usize, parity: bool) -> Manifest {
        let coding = parity.then(|| Coding {
            algorithm: RS_ALGORITHM.into(),
            data_shards: 2,
            parity_shards: 1,
            stripe_size: EC_STRIPE_SIZE,
        });
        let mut shards = Vec::new();
        for n in 0..data_count {
            shards.push(Shard {
                index: n as u32,
                offset: n as u64 * 4,
                size: 4,
                remote: "crypt:".into(),
                object: format!("crypt:old/d{n}"),
                blake3: blake3::hash(&[n as u8; 4]).to_hex().to_string(),
                kind: ShardKind::Data,
                group: if parity { n as u32 / 2 } else { 0 },
                slot: if parity { (n % 2) as u16 } else { 0 },
            });
        }
        if parity {
            for group in 0..data_count.div_ceil(2) {
                shards.push(Shard {
                    index: (data_count + group) as u32,
                    offset: 0,
                    size: 4,
                    remote: "crypt:".into(),
                    object: format!("crypt:old/p{group}"),
                    blake3: blake3::hash(&[9; 4]).to_hex().to_string(),
                    kind: ShardKind::Parity,
                    group: group as u32,
                    slot: 2,
                });
            }
        }
        Manifest {
            version: 2,
            archive_id: "test".into(),
            original_name: "file".into(),
            original_size: data_count as u64 * 4,
            shard_size: 4,
            created_unix: 1,
            content_root_blake3: crate::manifest::content_root_v2(
                data_count as u64 * 4,
                4,
                &coding,
                &shards,
            ),
            coding,
            shards,
        }
    }
    #[test]
    fn rs_composition_rebases_parity_and_preserves_old_group() {
        let base = manifest(4, true);
        let mut part = manifest(2, true);
        for s in &mut part.shards {
            s.object = s.object.replace("old", "new");
        }
        part.content_root_blake3 = crate::manifest::content_root_v2(
            part.original_size,
            part.shard_size,
            &part.coding,
            &part.shards,
        );
        let mapped = rebase(&part, &base, 1, 8).unwrap();
        assert_eq!(
            mapped.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![2, 3, 5]
        );
        assert_eq!(
            mapped.iter().map(|s| s.offset).collect::<Vec<_>>(),
            vec![8, 12, 0]
        );
        let mut final_manifest = base.clone();
        final_manifest.shards.retain(|s| s.group == 0);
        final_manifest.shards.extend(mapped);
        final_manifest.shards.sort_by_key(|s| s.index);
        final_manifest.content_root_blake3 = crate::manifest::content_root_v2(
            final_manifest.original_size,
            4,
            &final_manifest.coding,
            &final_manifest.shards,
        );
        crate::manifest::validate_manifest(&final_manifest).unwrap();
        assert!(final_manifest.shards[0].object.contains("old"));
        assert!(final_manifest.shards[2].object.contains("new"));
    }
    #[test]
    fn plain_and_partial_rs_groups_have_global_coordinates() {
        let plain = rebase(&manifest(1, false), &manifest(3, false), 2, 8).unwrap();
        assert_eq!((plain[0].index, plain[0].offset, plain[0].group), (2, 8, 0));
        let tail = rebase(&manifest(1, true), &manifest(3, true), 1, 8).unwrap();
        assert_eq!((tail[0].index, tail[1].index, tail[1].slot), (2, 4, 2));
        assert!(rebase(&manifest(1, false), &manifest(3, true), 1, 8).is_err());
    }
}
