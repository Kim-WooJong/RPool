//! Clean immutable cache; dirty spool never enters this directory.
//! The limit bounds clean payload plus conservative restore working space.
//! Zero disables admission (nonempty reads fail); unknown files are untouched.
use crate::prelude::*;
use crate::storage::reader::{is_restore_unavailable, StorageReader};

pub(crate) struct ShardCache {
    pub root: PathBuf,
    pub limit: u64,
    gate: Mutex<()>,
    verified: Mutex<BTreeMap<PathBuf, (u64, SystemTime)>>,
}
impl ShardCache {
    pub(crate) fn new(root: PathBuf, limit: u64) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let cache = Self {
            root,
            limit,
            gate: Mutex::new(()),
            verified: Mutex::new(BTreeMap::new()),
        };
        cache.cleanup()?;
        Ok(cache)
    }
    fn path(&self, s: &Shard) -> PathBuf {
        self.root.join(format!("{}-{}", s.blake3, s.size))
    }
    fn valid(&self, s: &Shard) -> Result<bool> {
        let path = self.path(s);
        if !path.exists() {
            return Ok(false);
        }
        let m = fs::symlink_metadata(&path)?;
        if !m.is_file() || m.file_type().is_symlink() {
            bail!("invalid cache entry");
        }
        let fingerprint = (m.len(), m.modified()?);
        if m.len() != s.size {
            return Ok(false);
        }
        if self.verified.lock().unwrap().get(&path) == Some(&fingerprint) {
            return Ok(true);
        }
        let valid = crate::utils::hash_file_range(&path, 0, s.size)? == s.blake3;
        if valid {
            self.verified.lock().unwrap().insert(path, fingerprint);
        }
        Ok(valid)
    }
    fn publish(&self, s: &Shard, source: &Path, offset: u64) -> Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        let mut input = File::open(source)?;
        input.seek(SeekFrom::Start(offset))?;
        let n = std::io::copy(&mut input.take(s.size), &mut temp)?;
        if n != s.size || crate::utils::hash_file_range(temp.path(), 0, s.size)? != s.blake3 {
            bail!("invalid recovered cache bytes");
        }
        temp.as_file().sync_all()?;
        temp.persist(self.path(s)).map_err(|e| e.error)?;
        self.remember_verified(s)?;
        Ok(())
    }
    fn remember_verified(&self, shard: &Shard) -> Result<()> {
        let path = self.path(shard);
        let metadata = fs::metadata(&path)?;
        self.verified
            .lock()
            .unwrap()
            .insert(path, (metadata.len(), metadata.modified()?));
        Ok(())
    }
    fn ensure(
        &self,
        reader: &StorageReader,
        m: &Manifest,
        s: &Shard,
        workers: usize,
        retries: u32,
    ) -> Result<()> {
        if self.valid(s)? {
            return Ok(());
        }
        self.reserve_locked(s.size)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        match reader.verified_read(s, &mut temp) {
            Ok(()) => {
                temp.as_file().sync_all()?;
                temp.persist(self.path(s)).map_err(|e| e.error)?;
                self.remember_verified(s)?;
                Ok(())
            }
            Err(e) if is_restore_unavailable(&e) && m.coding.is_some() => {
                let mini = group_manifest(m, s.group)?;
                // Drop the failed direct-read bytes before reserving recovery space.
                drop(temp);
                let manifest_bytes = serde_json::to_vec_pretty(&mini)?.len() as u64;
                // Restore holds the group output, staged shard attempts (including
                // failed attempts), and published data concurrently. Include bounded
                // resume JSON and its atomic replacement, even outside this root.
                let staged = mini.shards.iter().try_fold(0u64, |n, shard| {
                    n.checked_add(
                        shard
                            .size
                            .checked_mul(retries.max(1) as u64)
                            .context("recovery cache size overflow")?,
                    )
                    .context("recovery cache size overflow")
                })?;
                let required = mini
                    .original_size
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(staged))
                    .and_then(|n| n.checked_add(manifest_bytes))
                    .and_then(|n| n.checked_add(8192 + mini.shards.len() as u64 * 32))
                    .context("recovery cache size overflow")?;
                self.reserve_locked(required)?;
                let stage = tempfile::tempdir_in(&self.root)?;
                let manifest = stage.path().join("manifest.json");
                super::namespace::durable_json(&manifest, &mini)?;
                let output = stage.path().join("group");
                crate::commands::get_with_storage(
                    reader,
                    &manifest.to_string_lossy(),
                    &output,
                    workers,
                    retries,
                )?;
                for shard in crate::manifest::data_shards(&mini) {
                    self.publish(shard, &output, shard.offset)?;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
    pub(crate) fn read(
        &self,
        reader: &StorageReader,
        m: &Manifest,
        offset: u64,
        count: usize,
        workers: usize,
        retries: u32,
    ) -> Result<Vec<u8>> {
        crate::manifest::validate_manifest(m)?;
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow!("cache lock poisoned"))?;
        let outcome = (|| -> Result<Vec<u8>> {
            self.trim_locked()?;
            if count == 0 || offset >= m.original_size {
                return Ok(vec![]);
            }
            let count = (count as u64).min(m.original_size - offset) as usize;
            let end = offset.checked_add(count as u64).context("range overflow")?;
            let mut result = Vec::with_capacity(count);
            for s in crate::manifest::data_shards(m) {
                let shard_end = s
                    .offset
                    .checked_add(s.size)
                    .context("shard range overflow")?;
                if s.offset >= end || shard_end <= offset {
                    continue;
                }
                self.ensure(reader, m, s, workers, retries)?;
                let start = offset.max(s.offset);
                let n = (end.min(shard_end) - start) as usize;
                let mut f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(self.path(s))?;
                f.seek(SeekFrom::Start(start - s.offset))?;
                let old = result.len();
                result.resize(old + n, 0);
                f.read_exact(&mut result[old..])?;
                // Explicit access updates are independent of OS noatime/relatime.
                f.set_times(std::fs::FileTimes::new().set_accessed(SystemTime::now()))?;
            }
            if result.len() != count {
                bail!("incomplete verified range");
            }
            Ok(result)
        })();
        let cleanup = self.trim_locked();
        match outcome {
            Ok(bytes) => {
                cleanup?;
                Ok(bytes)
            }
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
        }
    }
    pub(crate) fn cleanup(&self) -> Result<u64> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow!("cache lock poisoned"))?;
        self.trim_locked()
    }
    fn trim_locked(&self) -> Result<u64> {
        self.reserve_locked(0)
    }
    fn reserve_locked(&self, required: u64) -> Result<u64> {
        if required > self.limit {
            bail!(
                "cache limit {} bytes is smaller than required working set {required} bytes",
                self.limit
            );
        }
        let target = self.limit - required;
        let mut entries = vec![];
        let mut total = 0u64;
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let name = item.file_name().to_string_lossy().into_owned();
            let Some((hash, size)) = name.split_once('-') else {
                continue;
            };
            if hash.len() != 64
                || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                || size.parse::<u64>().is_err()
            {
                continue;
            }
            let m = fs::symlink_metadata(item.path())?;
            if m.is_file() && !m.file_type().is_symlink() {
                total = total.saturating_add(m.len());
                entries.push((m.accessed()?, item.path(), m.len()));
            }
        }
        entries.sort();
        let mut removed = 0;
        for (_, path, size) in entries {
            if total <= target {
                break;
            }
            fs::remove_file(&path)?;
            self.verified.lock().unwrap().remove(&path);
            total -= size;
            removed += size;
        }
        Ok(removed)
    }
}
fn group_manifest(m: &Manifest, group: u32) -> Result<Manifest> {
    let coding = m
        .coding
        .clone()
        .context("plain archive has no recovery group")?;
    let mut shards = vec![];
    let mut offset = 0u64;
    for old in crate::manifest::data_shards(m)
        .into_iter()
        .filter(|s| s.group == group)
    {
        let mut s = old.clone();
        s.index = shards.len() as u32;
        s.offset = offset;
        s.group = 0;
        offset += s.size;
        shards.push(s);
    }
    let data_count = shards.len();
    for old in m
        .shards
        .iter()
        .filter(|s| s.group == group && s.kind == ShardKind::Parity)
    {
        let mut s = old.clone();
        s.index = (data_count + s.slot as usize - coding.data_shards) as u32;
        s.group = 0;
        shards.push(s);
    }
    let root =
        crate::manifest::content_root_v2(offset, m.shard_size, &Some(coding.clone()), &shards);
    let mini = Manifest {
        version: 2,
        archive_id: format!("{}-group-{group}", m.archive_id),
        original_name: m.original_name.clone(),
        original_size: offset,
        shard_size: m.shard_size,
        created_unix: m.created_unix,
        content_root_blake3: root,
        coding: Some(coding),
        shards,
    };
    crate::manifest::validate_manifest(&mini)?;
    Ok(mini)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{content_root_v1, content_root_v2, validate_manifest};
    use crate::storage::memory::faults::{FaultBackend, Rule};
    use crate::storage::memory::MemoryBackend;
    use crate::storage::reference::{BackendId, ObjectKey, ObjectRef};
    use crate::storage::registry::BackendRegistry;
    use crate::storage::traits::{OperationContext, StorageBackend, WriteOptions};
    use std::time::Duration;
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
    fn reads_only_intersecting_shards_and_reuses_verified_cache() {
        let f = Fixture::new(false);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 100).unwrap();
        f.delete(0);
        f.delete(2);
        assert_eq!(
            cache
                .read(&f.reader(vec![]), &f.manifest, 5, 2, 2, 0)
                .unwrap(),
            b"FG"
        );
        f.delete(1);
        assert_eq!(
            cache
                .read(&f.reader(vec![]), &f.manifest, 4, 4, 2, 0)
                .unwrap(),
            b"EFGH"
        );
        fs::write(cache.path(&f.manifest.shards[1]), b"xxxx").unwrap();
        assert!(cache
            .read(&f.reader(vec![]), &f.manifest, 4, 4, 2, 0)
            .is_err());
    }
    #[test]
    fn final_nonzero_group_recovers_without_unrelated_group() {
        let f = Fixture::new(true);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 20000).unwrap();
        for index in [0, 1, 2, 3, 4] {
            f.delete(index);
        }
        assert_eq!(
            cache
                .read(&f.reader(vec![]), &f.manifest, 8, 1, 4, 0)
                .unwrap(),
            b"I"
        );
        assert!(cache
            .read(&f.reader(vec![]), &f.manifest, 0, 1, 4, 0)
            .is_err());
    }
    #[test]
    fn access_recency_survives_reopen_and_evicted_bytes_redownload() {
        let f = Fixture::new(false);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 8).unwrap();
        let reader = f.reader(vec![]);
        cache.read(&reader, &f.manifest, 0, 8, 2, 0).unwrap();
        let first = cache.path(&f.manifest.shards[0]);
        let second = cache.path(&f.manifest.shards[1]);
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&first)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(old))
            .unwrap();
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&second)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_accessed(old + Duration::from_secs(1)))
            .unwrap();
        cache.read(&reader, &f.manifest, 0, 1, 2, 0).unwrap();
        assert!(fs::metadata(&first).unwrap().accessed().unwrap() > old);
        drop(cache);
        let cache = ShardCache::new(temp.path().into(), 8).unwrap();
        cache.read(&reader, &f.manifest, 8, 1, 2, 0).unwrap();
        assert!(first.exists());
        assert!(!second.exists());
        assert_eq!(
            cache.read(&reader, &f.manifest, 4, 4, 2, 0).unwrap(),
            b"EFGH"
        );
        let bytes: u64 = fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes <= 8);
    }

    #[test]
    fn reopening_with_lower_limit_trims_clean_data_without_a_read() {
        let f = Fixture::new(false);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 8).unwrap();
        cache
            .read(&f.reader(vec![]), &f.manifest, 0, 8, 2, 0)
            .unwrap();
        fs::write(temp.path().join("unknown-dirty"), b"preserve").unwrap();
        drop(cache);
        let _cache = ShardCache::new(temp.path().into(), 0).unwrap();
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read(temp.path().join("unknown-dirty")).unwrap(),
            b"preserve"
        );
    }

    #[test]
    fn recovery_rejects_insufficient_working_set_and_zero_read_is_empty() {
        let f = Fixture::new(true);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 4).unwrap();
        f.delete(0);
        let error = cache
            .read(&f.reader(vec![]), &f.manifest, 0, 1, 2, 0)
            .unwrap_err();
        assert!(error.to_string().contains("required working set"));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
        let cache = ShardCache::new(temp.path().into(), 0).unwrap();
        assert!(cache
            .read(&f.reader(vec![]), &f.manifest, 1, 0, 2, 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn eviction_removes_only_clean_entries_not_unknown_files() {
        let f = Fixture::new(false);
        let temp = tempfile::tempdir().unwrap();
        let cache = ShardCache::new(temp.path().into(), 0).unwrap();
        fs::write(temp.path().join("dirty-spool"), b"keep").unwrap();
        assert!(cache
            .read(&f.reader(vec![]), &f.manifest, 0, 2, 2, 0)
            .unwrap_err()
            .to_string()
            .contains("required working set"));
        assert!(!cache.path(&f.manifest.shards[0]).exists());
        assert_eq!(fs::read(temp.path().join("dirty-spool")).unwrap(), b"keep");
    }
}
