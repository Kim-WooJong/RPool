//! Concurrency, single-flight, readahead, prefix serving and eviction tests
//! on an instrumented in-memory backend (no cloud, no real rclone).
use super::*;
use crate::storage::capabilities::BackendCapabilities;
use crate::storage::error::StorageError;
use crate::storage::memory::MemoryBackend;
use crate::storage::reference::{BackendId, ObjectKey, ObjectRef};
use crate::storage::registry::BackendRegistry;
use crate::storage::traits::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Wraps a memory backend, counts reads per key and records how reads of
/// rendezvous keys overlap. `hold` = (key, n, corrupt): that key delivers its
/// first n bytes, then blocks until released; with `corrupt` the first byte
/// delivered after the hold is flipped.
struct Probe {
    inner: MemoryBackend,
    /// Concurrent reads of rendezvous keys only.
    meeting: AtomicUsize,
    met: AtomicUsize,
    reads: Mutex<BTreeMap<String, usize>>,
    /// Reads of these keys wait (up to 3 s) until that many of them overlap.
    rendezvous: (BTreeSet<String>, usize),
    hold: Option<(String, usize, bool)>,
    released: AtomicBool,
    signal: Mutex<()>,
    wake: Condvar,
}

impl Probe {
    fn new(id: BackendId) -> Self {
        Self {
            inner: MemoryBackend::new(id),
            meeting: AtomicUsize::new(0),
            met: AtomicUsize::new(0),
            reads: Mutex::new(BTreeMap::new()),
            rendezvous: (BTreeSet::new(), 0),
            hold: None,
            released: AtomicBool::new(false),
            signal: Mutex::new(()),
            wake: Condvar::new(),
        }
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }
    fn reads(&self, key: &str) -> usize {
        self.reads.lock().unwrap().get(key).copied().unwrap_or(0)
    }
    fn wait_until(&self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut guard = self.signal.lock().unwrap();
        while !done(self) && Instant::now() < deadline {
            guard = self
                .wake
                .wait_timeout(guard, Duration::from_millis(10))
                .unwrap()
                .0;
        }
    }
}

impl StorageBackend for Probe {
    fn id(&self) -> BackendId {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    fn stat(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
    ) -> Result<ObjectMetadata, StorageError> {
        self.inner.stat(ctx, key)
    }
    fn read(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        range: &ReadRange,
        sink: &mut dyn Write,
    ) -> Result<ReadReceipt, StorageError> {
        let name = key.as_str().to_owned();
        *self.reads.lock().unwrap().entry(name.clone()).or_default() += 1;
        let meets = self.rendezvous.0.contains(&name);
        if meets {
            let now = self.meeting.fetch_add(1, Ordering::SeqCst) + 1;
            self.met.fetch_max(now, Ordering::SeqCst);
            self.wake.notify_all();
            self.wait_until(|p| p.met.load(Ordering::SeqCst) >= p.rendezvous.1);
        }
        let result = (|| {
            let bytes = self.inner.read_all(ctx, key, None)?;
            let start = (range.offset() as usize).min(bytes.len());
            let end = (range.end() as usize).min(bytes.len());
            let mut bytes = bytes[start..end].to_vec();
            let io = |e: std::io::Error| StorageError::Other {
                detail: e.to_string(),
            };
            let mut split = 0;
            if let Some((held, after, corrupt)) = &self.hold {
                if *held == name {
                    split = (*after).min(bytes.len());
                    sink.write_all(&bytes[..split]).map_err(io)?;
                    self.wait_until(|p| p.released.load(Ordering::SeqCst));
                    if *corrupt && split < bytes.len() {
                        bytes[split] ^= 1;
                    }
                }
            }
            sink.write_all(&bytes[split..]).map_err(io)?;
            Ok(ReadReceipt {
                bytes_read: bytes.len() as u64,
                version: None,
            })
        })();
        if meets {
            self.meeting.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }
    fn read_all(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        limit: Option<usize>,
    ) -> Result<Vec<u8>, StorageError> {
        self.inner.read_all(ctx, key, limit)
    }
    fn write(
        &self,
        ctx: &OperationContext,
        key: &ObjectKey,
        source: &mut dyn Read,
        options: &WriteOptions,
    ) -> Result<WriteReceipt, StorageError> {
        self.inner.write(ctx, key, source, options)
    }
    fn delete(&self, ctx: &OperationContext, key: &ObjectKey) -> Result<(), StorageError> {
        self.inner.delete(ctx, key)
    }
    fn list(
        &self,
        ctx: &OperationContext,
        prefix: &str,
        page: Option<&str>,
    ) -> Result<ListPage, StorageError> {
        self.inner.list(ctx, prefix, page)
    }
    fn copy(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<CopyReceipt, StorageError> {
        self.inner.copy(ctx, source, destination)
    }
    fn rename(
        &self,
        ctx: &OperationContext,
        source: &ObjectKey,
        destination: &ObjectKey,
    ) -> Result<(), StorageError> {
        self.inner.rename(ctx, source, destination)
    }
}

/// A plain (uncoded) archive whose data shards are `payloads`, all of one
/// size except possibly the last, each on its own remote alias.
struct Plain {
    probe: Arc<Probe>,
    manifest: Manifest,
    reader: Arc<StorageReader>,
}

fn plain(payloads: &[&[u8]], configure: impl FnOnce(&mut Probe)) -> Plain {
    let id = BackendId::new("probe").unwrap();
    let mut probe = Probe::new(id.clone());
    configure(&mut probe);
    let mut bindings = BTreeMap::new();
    let mut shards = vec![];
    let mut offset = 0u64;
    for (index, bytes) in payloads.iter().enumerate() {
        let key = ObjectKey::new(format!("s{index}")).unwrap();
        probe
            .inner
            .write(
                &OperationContext::none(),
                &key,
                &mut std::io::Cursor::new(bytes),
                &WriteOptions::default(),
            )
            .unwrap();
        let object = format!("acct{index}:archive/{index}");
        bindings.insert(object.clone(), ObjectRef::new(id.clone(), key));
        shards.push(Shard {
            index: index as u32,
            offset,
            size: bytes.len() as u64,
            remote: format!("acct{index}:"),
            object,
            blake3: blake3::hash(bytes).to_hex().to_string(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        });
        offset += bytes.len() as u64;
    }
    let manifest = Manifest {
        version: 1,
        archive_id: "probe".into(),
        original_name: "input".into(),
        original_size: offset,
        shard_size: payloads[0].len() as u64,
        created_unix: 0,
        content_root_blake3: crate::manifest::content_root_v1(&shards),
        coding: None,
        shards,
    };
    crate::manifest::validate_manifest(&manifest).unwrap();
    let probe = Arc::new(probe);
    let mut registry = BackendRegistry::new();
    let backend: Arc<dyn StorageBackend> = probe.clone();
    registry.register(backend).unwrap();
    let reader = Arc::new(StorageReader::from_registry(
        registry,
        bindings,
        OperationContext::none(),
    ));
    Plain {
        probe,
        manifest,
        reader,
    }
}

fn eventually(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn reads_of_different_shards_overlap_instead_of_serializing() {
    let f = plain(&[b"ABCD", b"EFGH"], |p| {
        p.rendezvous = (["s0".into(), "s1".into()].into(), 2);
    });
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| cache.read(&f.reader, &f.manifest, 0, 4, 2, 0).unwrap());
        let b = scope.spawn(|| cache.read(&f.reader, &f.manifest, 4, 4, 2, 0).unwrap());
        assert_eq!(a.join().unwrap(), b"ABCD");
        assert_eq!(b.join().unwrap(), b"EFGH");
    });
    assert_eq!(f.probe.met.load(Ordering::SeqCst), 2);
}

#[test]
fn readers_of_one_missing_shard_share_a_single_download() {
    let f = plain(&[b"ABCDEFGH"], |p| p.hold = Some(("s0".into(), 4, false)));
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| cache.read_shared(&f.reader, &f.manifest, 0, 8, 2, 0)))
            .collect();
        assert!(eventually(|| f.probe.reads("s0") == 1));
        std::thread::sleep(Duration::from_millis(50));
        f.probe.release();
        for reader in readers {
            assert_eq!(reader.join().unwrap().unwrap(), b"ABCDEFGH");
        }
    });
    assert_eq!(f.probe.reads("s0"), 1);
    assert!(cache.path(&f.manifest.shards[0]).exists());
}

#[test]
fn first_bytes_are_served_before_the_whole_shard_arrives() {
    let f = plain(&[b"ABCDEFGH"], |p| p.hold = Some(("s0".into(), 4, false)));
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    // The download is still blocked after 4 bytes; the prefix is served.
    assert_eq!(
        cache
            .read_shared(&f.reader, &f.manifest, 1, 2, 2, 0)
            .unwrap(),
        b"BC"
    );
    assert!(!cache.path(&f.manifest.shards[0]).exists());
    f.probe.release();
    // A range that reaches the shard end waits for the verified entry.
    assert_eq!(
        cache
            .read_shared(&f.reader, &f.manifest, 2, 6, 2, 0)
            .unwrap(),
        b"CDEFGH"
    );
    assert!(cache.path(&f.manifest.shards[0]).exists());
    assert_eq!(f.probe.reads("s0"), 1);
}

#[test]
fn failed_verification_after_a_served_prefix_publishes_nothing() {
    let f = plain(&[b"ABCDEFGH"], |p| p.hold = Some(("s0".into(), 4, true)));
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    assert_eq!(
        cache
            .read_shared(&f.reader, &f.manifest, 0, 2, 2, 0)
            .unwrap(),
        b"AB"
    );
    f.probe.release();
    // Plain archives cannot reconstruct: the corrupt shard is an error, and
    // neither an entry nor a partial file is left behind.
    assert!(cache
        .read_shared(&f.reader, &f.manifest, 0, 8, 2, 0)
        .is_err());
    assert!(eventually(|| cache.inner.state().flights.is_empty()));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn sequential_reads_prefetch_following_shards_in_parallel() {
    let f = plain(&[b"AAAA", b"BBBB", b"CCCC", b"DDDD", b"EEEE"], |p| {
        p.rendezvous = (["s2".into(), "s3".into()].into(), 2);
    });
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 1000).unwrap();
    let read = |offset, count| {
        cache
            .read_shared(&f.reader, &f.manifest, offset, count, 2, 0)
            .unwrap()
    };
    assert_eq!(read(0, 4), b"AAAA");
    // A first read is not a stream yet: nothing is fetched ahead.
    assert_eq!(f.probe.reads("s1"), 0);
    assert_eq!(read(4, 4), b"BBBB");
    // Continuing read: the next `workers` shards come ahead, concurrently.
    assert!(eventually(|| {
        cache.path(&f.manifest.shards[2]).exists() && cache.path(&f.manifest.shards[3]).exists()
    }));
    // Both readahead downloads were in progress at the same time.
    assert_eq!(f.probe.met.load(Ordering::SeqCst), 2);
    assert_eq!(read(8, 8), b"CCCCDDDD");
    assert_eq!((f.probe.reads("s2"), f.probe.reads("s3")), (1, 1));
}

#[test]
fn readahead_never_evicts_recently_used_entries() {
    let f = plain(&[b"AAAA", b"BBBB", b"CCCC", b"DDDD", b"EEEE"], |_| {});
    let temp = tempfile::tempdir().unwrap();
    // Four entries fit; readahead plans one shard (a quarter of the limit).
    let cache = ShardCache::new(temp.path().into(), 16).unwrap();
    let read = |offset, count| {
        let bytes = cache
            .read_shared(&f.reader, &f.manifest, offset, count, 2, 0)
            .unwrap();
        assert!(eventually(|| cache.inner.state().flights.is_empty()));
        bytes
    };
    assert_eq!(read(0, 4), b"AAAA");
    assert_eq!(read(4, 4), b"BBBB");
    assert_eq!(f.probe.reads("s2"), 1);
    assert_eq!(read(8, 4), b"CCCC");
    assert_eq!(f.probe.reads("s3"), 1);
    // Shard 4 would need an eviction, but every entry is recently used.
    assert_eq!(read(12, 4), b"DDDD");
    assert_eq!(f.probe.reads("s4"), 0);
    for s in &f.manifest.shards[..4] {
        assert!(cache.path(s).exists());
    }
    // A demand read does evict the least recently used entry.
    assert_eq!(read(16, 4), b"EEEE");
    assert!(!cache.path(&f.manifest.shards[0]).exists());
    for s in &f.manifest.shards[1..] {
        assert!(cache.path(s).exists());
    }
}

#[test]
fn disk_pressure_evicts_only_idle_clean_entries() {
    let f = plain(&[b"AAAA", b"BBBB"], |_| {});
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    cache.read(&f.reader, &f.manifest, 0, 8, 2, 0).unwrap();
    let old = SystemTime::now() - Duration::from_secs(3600);
    OpenOptions::new()
        .write(true)
        .open(cache.path(&f.manifest.shards[0]))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(old))
        .unwrap();
    fs::write(temp.path().join("dirty-spool"), b"keep").unwrap();
    drop(cache);
    let mut cache = ShardCache::new(temp.path().into(), 100).unwrap();
    // Pretend the disk is always below its free floor.
    Arc::get_mut(&mut cache.inner).unwrap().disk_floor = u64::MAX / 2;
    assert_eq!(cache.relieve_disk(1).unwrap(), 4);
    assert!(!cache.path(&f.manifest.shards[0]).exists());
    assert!(cache.path(&f.manifest.shards[1]).exists());
    assert_eq!(cache.relieve_disk(1).unwrap(), 0);
    assert_eq!(fs::read(temp.path().join("dirty-spool")).unwrap(), b"keep");
}

#[test]
fn pinned_entries_survive_demand_eviction() {
    let f = plain(&[b"AAAA", b"BBBB"], |_| {});
    let temp = tempfile::tempdir().unwrap();
    let cache = ShardCache::new(temp.path().into(), 4).unwrap();
    cache.read(&f.reader, &f.manifest, 0, 4, 2, 0).unwrap();
    let name = entry_name(&f.manifest.shards[0]);
    assert!(cache.inner.state().index.pin(&name));
    // The only entry is pinned, so shard 1 cannot get space; the wait is
    // released by unpinning from another thread.
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| cache.read(&f.reader, &f.manifest, 4, 4, 2, 0));
        std::thread::sleep(Duration::from_millis(100));
        assert!(cache.path(&f.manifest.shards[0]).exists());
        cache.inner.state().index.unpin(&name, None);
        cache.inner.space.notify_all();
        assert_eq!(reader.join().unwrap().unwrap(), b"BBBB");
    });
    assert!(!cache.path(&f.manifest.shards[0]).exists());
}

#[test]
fn crash_leftover_partials_are_removed_at_open() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(format!("{PARTIAL_PREFIX}abc")), b"xyz").unwrap();
    fs::write(temp.path().join("unknown"), b"keep").unwrap();
    let cache = ShardCache::new(temp.path().into(), 100).unwrap();
    assert_eq!(cache.startup_removed_bytes, 3);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}
