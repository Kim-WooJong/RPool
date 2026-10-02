//! Clean immutable cache; dirty spool never enters this directory.
//! The limit bounds clean payload plus conservative restore working space.
//! Zero disables admission (nonempty reads fail); unknown files are untouched.
//!
//! Concurrency: there is no global read lock. A short-held state mutex guards
//! the LRU index, the space reservations and the table of in-flight shard
//! downloads; every missing shard has exactly one flight that all of its
//! readers share (single-flight). Published entries are verified (BLAKE3) and
//! immutable; a download only becomes an entry after full verification.
//!
//! Eviction is one LRU policy over clean entries only: entries pinned by an
//! open read and bytes reserved by in-flight downloads are never evicted, and
//! readahead may only evict entries idle for a while (see `index`).
//!
//! First-byte latency (`read_shared`): a missing shard is downloaded in the
//! background into a partial file, and a reader whose range is already in that
//! partial file is served from it before the whole shard is verified.
//! Integrity trade-off: such bytes are not yet BLAKE3-verified by RPool. They
//! are the exact bytes that later get hashed, and pool remotes are rclone crypt
//! remotes, which authenticate every 64 KiB block with the key (Poly1305), so
//! forged or bit-flipped bytes are rejected by rclone before they reach us;
//! what crypt cannot rule out is a substituted object encrypted under the same
//! key. A failed verification never publishes an entry: it is logged, the
//! partial file is dropped, and later reads reconstruct the shard. A range that
//! reaches the end of its shard always waits for verification, and the
//! inline `read` (used by migration/recovery copies) never serves a prefix.
//!
//! Entry points: [`ShardCache::new`], `read` / `read_shared`, `cleanup` and
//! `relieve_disk`; owned by `VirtualDrive` (and temporary caches in recovery,
//! transition and adoption).
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use std::collections::HashMap;
use std::sync::Condvar;
use std::time::{Duration, Instant};

mod disk;
mod fetch;
mod flight;
mod index;
mod prefetch;

#[cfg(test)]
mod concurrency_tests;

use fetch::Job;
use flight::{Flight, Outcome, Ready};
use index::{Admission, Index};

pub(crate) use disk::DISK_FLOOR;

/// Name prefix of partial downloads; leftovers of a crash are removed at open.
const PARTIAL_PREFIX: &str = ".rpool-partial-";
/// Longest a demand read waits for reserved or pinned space to be released.
const SPACE_WAIT: Duration = Duration::from_secs(120);
/// Share of the cache limit readahead may plan to fill at once.
const PREFETCH_SHARE: u64 = 4;

/// Cache file name of a shard: `<blake3>-<size>`.
fn entry_name(s: &Shard) -> String {
    format!("{}-{}", s.blake3, s.size)
}

/// Mutable cache state guarded by `Inner::state`.
struct State {
    /// LRU index of clean entries, pins and reservations.
    index: Index,
    /// In-flight downloads by entry name (one per missing shard).
    flights: HashMap<String, Arc<Flight>>,
    /// Recent read streams, for sequential readahead.
    sequential: prefetch::Sequential,
    /// Readahead (prefetch) flights currently running; bounded by `workers`.
    prefetching: usize,
}

/// Shared cache internals, referenced by background download threads.
struct Inner {
    /// Cache directory.
    root: PathBuf,
    /// Byte limit for clean entries plus reservations (0 disables admission).
    limit: u64,
    /// Free disk space relief tries to keep (see `disk::DISK_FLOOR`).
    disk_floor: u64,
    /// Serve verified-later prefixes of background downloads (module docs).
    serve_prefix: bool,
    /// Index, flights and readahead state.
    state: Mutex<State>,
    /// Signalled whenever reserved, pinned or published bytes change.
    space: Condvar,
    /// Shard files already hashed, by path -> (size, mtime), so unchanged
    /// entries are not rehashed on every hit.
    verified: Mutex<BTreeMap<PathBuf, (u64, SystemTime)>>,
}

/// Bytes held for an in-flight download or restore; released on drop.
struct Reservation<'a> {
    /// Cache whose reservation is released.
    inner: &'a Inner,
    /// Reserved bytes.
    bytes: u64,
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.inner.state().index.release(self.bytes);
        self.inner.space.notify_all();
    }
}

/// How a read obtains missing shards.
#[derive(Clone, Copy)]
enum Fetch<'a> {
    /// The calling thread downloads; only verified bytes are returned.
    Inline(&'a StorageReader),
    /// Background downloads with prefix serving and readahead.
    Background(&'a Arc<StorageReader>),
}

/// Verified clean shard cache of one drive workspace (see module docs).
pub(crate) struct ShardCache {
    /// Bytes removed when opening (over-limit entries and crash leftovers),
    /// for the startup report.
    pub(crate) startup_removed_bytes: u64,
    /// Shared state, also held by background downloads.
    inner: Arc<Inner>,
}

impl ShardCache {
    /// Opens the cache at `root` with byte `limit`: removes partial downloads,
    /// indexes existing entries and trims to the limit.
    pub(crate) fn new(root: PathBuf, limit: u64) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let partial_bytes = remove_partials(&root)?;
        let inner = Inner {
            state: Mutex::new(State {
                index: Index::scan(&root)?,
                flights: HashMap::new(),
                sequential: prefetch::Sequential::default(),
                prefetching: 0,
            }),
            root,
            limit,
            disk_floor: DISK_FLOOR,
            serve_prefix: true,
            space: Condvar::new(),
            verified: Mutex::new(BTreeMap::new()),
        };
        let removed = inner.trim()?;
        Ok(Self {
            startup_removed_bytes: removed.saturating_add(partial_bytes),
            inner: Arc::new(inner),
        })
    }
    #[cfg(test)]
    fn path(&self, s: &Shard) -> PathBuf {
        self.inner.path(s)
    }
    /// Read with verified bytes only; missing shards are fetched by the
    /// calling thread. For bulk copies (migration, recovery) and tests.
    pub(crate) fn read(
        &self,
        reader: &StorageReader,
        m: &Manifest,
        offset: u64,
        count: usize,
        workers: usize,
        retries: u32,
    ) -> Result<Vec<u8>> {
        self.read_with(Fetch::Inline(reader), m, offset, count, workers, retries)
    }
    /// Interactive read: missing shards of the range download in parallel in
    /// the background, the range may be served from a download's prefix, and
    /// sequential access starts readahead of the following shards.
    pub(crate) fn read_shared(
        &self,
        reader: &Arc<StorageReader>,
        m: &Manifest,
        offset: u64,
        count: usize,
        workers: usize,
        retries: u32,
    ) -> Result<Vec<u8>> {
        self.read_with(
            Fetch::Background(reader),
            m,
            offset,
            count,
            workers,
            retries,
        )
    }
    /// Shared body of `read` and `read_shared`: collects the data shards of
    /// the range, keeps background downloads ahead (`Background` only) and
    /// copies each shard's part into the result.
    fn read_with(
        &self,
        fetch: Fetch<'_>,
        m: &Manifest,
        offset: u64,
        count: usize,
        workers: usize,
        retries: u32,
    ) -> Result<Vec<u8>> {
        crate::manifest::validate_manifest(m)?;
        if count == 0 || offset >= m.original_size {
            return Ok(vec![]);
        }
        let count = (count as u64).min(m.original_size - offset) as usize;
        let end = offset.checked_add(count as u64).context("range overflow")?;
        let mut shards = vec![];
        for s in crate::manifest::data_shards(m) {
            let shard_end = s
                .offset
                .checked_add(s.size)
                .context("shard range overflow")?;
            if s.offset < end && shard_end > offset {
                shards.push(s);
            }
        }
        // Interactive reads keep up to `workers` shards of the range (within
        // half the cache budget) downloading in parallel ahead of the copy,
        // and start readahead past the range when its last shard is reached.
        let ahead = workers.max(1);
        let budget = (self.inner.limit / 2).max(1);
        let mut result = Vec::with_capacity(count);
        for (i, s) in shards.iter().enumerate() {
            if let Fetch::Background(reader) = fetch {
                let mut planned = 0u64;
                for (k, next) in shards[i..].iter().take(ahead).enumerate() {
                    planned = planned.saturating_add(next.size);
                    if k > 0 && planned > budget {
                        break;
                    }
                    self.inner
                        .start(reader, m, next, Admission::Demand, workers, retries)?;
                }
                if i + 1 == shards.len() {
                    self.inner
                        .readahead(reader, m, offset, end, workers, retries);
                }
            }
            let start = offset.max(s.offset);
            let n = (end.min(s.offset + s.size) - start) as usize;
            self.inner
                .read_shard(fetch, m, s, start, n, &mut result, workers, retries)?;
        }
        if result.len() != count {
            bail!("incomplete verified range");
        }
        Ok(result)
    }
    /// Evict least recently used clean entries down to the limit.
    pub(crate) fn cleanup(&self) -> Result<u64> {
        self.inner.trim()
    }
    /// Make room on disk for `incoming` local bytes (spool growth) by evicting
    /// clean entries that are not recently used, so the disk keeps its free
    /// floor. Never fails for lack of evictable entries; returns bytes freed.
    pub(crate) fn relieve_disk(&self, incoming: u64) -> Result<u64> {
        let mut st = self.inner.lock()?;
        self.inner
            .relieve_locked(&mut st, incoming, Admission::Prefetch)
    }
}

/// Remove partial downloads left by an earlier process. Returns their bytes.
fn remove_partials(root: &Path) -> Result<u64> {
    let mut removed = 0u64;
    for item in fs::read_dir(root)? {
        let item = item?;
        if !item
            .file_name()
            .to_string_lossy()
            .starts_with(PARTIAL_PREFIX)
        {
            continue;
        }
        let m = fs::symlink_metadata(item.path())?;
        if m.is_file() && !m.file_type().is_symlink() {
            fs::remove_file(item.path())?;
            removed = removed.saturating_add(m.len());
        }
    }
    Ok(removed)
}

impl Inner {
    /// Path of a shard's cache entry.
    fn path(&self, s: &Shard) -> PathBuf {
        self.root.join(entry_name(s))
    }
    /// Locks the state; a poisoned mutex is an error.
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("cache lock poisoned"))
    }
    /// For release paths that must not fail: the state stays consistent
    /// because every mutation under the lock is a plain counter/map update.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Locks the verified-fingerprint map, recovering from poisoning.
    fn verified(&self) -> std::sync::MutexGuard<'_, BTreeMap<PathBuf, (u64, SystemTime)>> {
        self.verified.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Whether the shard's entry exists with the right size and hash (cached
    /// fingerprint or a full rehash). A non-regular file is an error.
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
        if self.verified().get(&path) == Some(&fingerprint) {
            return Ok(true);
        }
        let valid = crate::utils::hash_file_range(&path, 0, s.size)? == s.blake3;
        if valid {
            self.verified().insert(path, fingerprint);
        }
        Ok(valid)
    }
    /// Records the current fingerprint of a just-verified entry.
    fn remember_verified(&self, shard: &Shard) -> Result<()> {
        let path = self.path(shard);
        let metadata = fs::metadata(&path)?;
        self.verified()
            .insert(path, (metadata.len(), metadata.modified()?));
        Ok(())
    }
    /// Index a verified entry that is now on disk.
    fn admit_entry(&self, s: &Shard) -> Result<()> {
        let accessed = fs::metadata(self.path(s))?
            .accessed()
            .unwrap_or_else(|_| SystemTime::now())
            .max(SystemTime::now());
        self.lock()?.index.insert(entry_name(s), s.size, accessed);
        self.space.notify_all();
        Ok(())
    }
    /// Evicts one entry allowed by `mode` and forgets its fingerprint; returns
    /// the bytes freed (`None`: nothing evictable).
    fn evict_one(&self, st: &mut State, mode: Admission) -> Result<Option<u64>> {
        let evicted = st.index.evict_one(&self.root, mode, SystemTime::now())?;
        Ok(evicted.map(|(path, size)| {
            self.verified().remove(&path);
            size
        }))
    }
    /// Evicts least recently used entries until usage is within `limit`;
    /// returns the bytes removed.
    fn trim(&self) -> Result<u64> {
        let mut st = self.lock()?;
        let mut removed = 0u64;
        while st.index.used() > self.limit {
            match self.evict_one(&mut st, Admission::Demand)? {
                Some(size) => removed += size,
                None => break,
            }
        }
        Ok(removed)
    }
    /// Evict until the disk has `incoming` bytes plus its free floor, as far
    /// as `mode` allows. Unknown free space means no relief.
    fn relieve_locked(&self, st: &mut State, incoming: u64, mode: Admission) -> Result<u64> {
        let Some(mut free) = disk::available(&self.root) else {
            return Ok(0);
        };
        let want = incoming.saturating_add(self.disk_floor);
        let mut removed = 0u64;
        while free < want {
            match self.evict_one(st, mode)? {
                Some(size) => {
                    free = free.saturating_add(size);
                    removed += size;
                }
                None => break,
            }
        }
        Ok(removed)
    }
    /// Reserve `need` bytes of the budget, evicting least recently used clean
    /// entries first. A demand read waits for in-flight or pinned bytes to be
    /// released; readahead gives up instead. Fails only when even evicting
    /// everything evictable cannot make room.
    fn reserve(&self, need: u64, mode: Admission) -> Result<Reservation<'_>> {
        if need > self.limit {
            bail!(
                "cache limit {} bytes is smaller than required working set {need} bytes",
                self.limit
            );
        }
        let deadline = Instant::now() + SPACE_WAIT;
        let mut st = self.lock()?;
        loop {
            while st.index.used() + need > self.limit {
                if self.evict_one(&mut st, mode)?.is_none() {
                    break;
                }
            }
            if st.index.used() + need <= self.limit {
                // Relieve before the guard exists: dropping a guard while
                // the state lock is held would deadlock.
                self.relieve_locked(&mut st, need, mode)?;
                st.index.reserve(need);
                return Ok(Reservation {
                    inner: self,
                    bytes: need,
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if mode == Admission::Prefetch || left.is_zero() {
                bail!(
                    "cache limit {} bytes is held by reads in progress; required working set {need} bytes",
                    self.limit
                );
            }
            st = self
                .space
                .wait_timeout(st, left)
                .map_err(|_| anyhow!("cache lock poisoned"))?
                .0;
        }
    }
    /// Register a flight for a shard that is neither cached nor in flight.
    fn begin(
        &self,
        m: &Manifest,
        s: &Shard,
        mode: Admission,
        workers: usize,
        retries: u32,
    ) -> Result<Option<(Arc<Flight>, Job)>> {
        let name = entry_name(s);
        let idle = |st: &State| !st.index.contains(&name) && !st.flights.contains_key(&name);
        if !idle(&*self.lock()?) {
            return Ok(None);
        }
        let job = Job::new(m, s, mode, workers, retries)?;
        let mut st = self.lock()?;
        if !idle(&st) || (mode == Admission::Prefetch && st.prefetching >= workers.max(1)) {
            return Ok(None);
        }
        let flight = Arc::new(Flight::new(mode));
        st.flights.insert(name, Arc::clone(&flight));
        if mode == Admission::Prefetch {
            st.prefetching += 1;
        }
        Ok(Some((flight, job)))
    }
    /// Start a background flight for `s` unless it is cached or in flight.
    fn start(
        self: &Arc<Self>,
        reader: &Arc<StorageReader>,
        m: &Manifest,
        s: &Shard,
        mode: Admission,
        workers: usize,
        retries: u32,
    ) -> Result<()> {
        let Some((flight, job)) = self.begin(m, s, mode, workers, retries)? else {
            return Ok(());
        };
        let (inner, reader, running) = (Arc::clone(self), Arc::clone(reader), Arc::clone(&flight));
        let spawned = std::thread::Builder::new()
            .name("rpool-shard-fetch".into())
            .spawn(move || {
                // Waiters receive the outcome through the flight.
                let _ = inner.run(&reader, &job, &running);
            });
        if let Err(error) = spawned {
            self.retire(
                s,
                &flight,
                Outcome::Failed {
                    message: format!("{error:#}"),
                    retry: false,
                },
            );
            return Err(error.into());
        }
        Ok(())
    }
    /// Sequential reads fetch the next shards ahead within a bounded window.
    /// Best effort: a readahead that cannot start is simply skipped.
    fn readahead(
        self: &Arc<Self>,
        reader: &Arc<StorageReader>,
        m: &Manifest,
        offset: u64,
        end: u64,
        workers: usize,
        retries: u32,
    ) {
        let key = prefetch::stream_key(m);
        if !self.state().sequential.observe(&key, offset, end) {
            return;
        }
        let budget = self.limit / PREFETCH_SHARE;
        for s in prefetch::window(m, end, workers.max(1), budget) {
            if self
                .start(reader, m, &s, Admission::Prefetch, workers, retries)
                .is_err()
            {
                break;
            }
        }
    }
    /// Append `[start, start + n)` of shard `s` to `out`.
    #[allow(clippy::too_many_arguments)]
    fn read_shard(
        self: &Arc<Self>,
        fetch: Fetch<'_>,
        m: &Manifest,
        s: &Shard,
        start: u64,
        n: usize,
        out: &mut Vec<u8>,
        workers: usize,
        retries: u32,
    ) -> Result<()> {
        enum Step {
            Cached,
            Join(Arc<Flight>),
            Missing,
        }
        let name = entry_name(s);
        let within = start - s.offset;
        let end = within + n as u64;
        // Prefix serving needs a background download and never covers the
        // shard's last byte, so every complete read ends verified.
        let early = self.serve_prefix && matches!(fetch, Fetch::Background(_)) && end < s.size;
        for _ in 0..8 {
            let step = {
                let mut st = self.lock()?;
                if let Some(flight) = st.flights.get(&name) {
                    Step::Join(Arc::clone(flight))
                } else if st.index.pin(&name) {
                    Step::Cached
                } else {
                    Step::Missing
                }
            };
            match step {
                Step::Cached => {
                    let copied = self.copy_cached(s, within, n, out);
                    let accessed = matches!(copied, Ok(true)).then(SystemTime::now);
                    let mut st = self.state();
                    st.index.unpin(&name, accessed);
                    if matches!(copied, Ok(false)) {
                        // Missing or invalid on disk: download it again.
                        st.index.forget(&name);
                    }
                    drop(st);
                    self.space.notify_all();
                    if copied? {
                        return Ok(());
                    }
                }
                Step::Missing => match fetch {
                    Fetch::Inline(reader) => {
                        if let Some((flight, job)) =
                            self.begin(m, s, Admission::Demand, workers, retries)?
                        {
                            self.run(reader, &job, &flight)?;
                        }
                    }
                    Fetch::Background(reader) => {
                        self.start(reader, m, s, Admission::Demand, workers, retries)?;
                    }
                },
                Step::Join(flight) => match flight.wait(end, early) {
                    Ready::Prefix(file) => {
                        let old = out.len();
                        out.resize(old + n, 0);
                        crate::utils::read_exact_at(&file, &mut out[old..], within)?;
                        return Ok(());
                    }
                    Ready::Done(Outcome::Published)
                    | Ready::Done(Outcome::Failed { retry: true, .. }) => {}
                    Ready::Done(Outcome::Failed { message, .. }) => bail!("{message}"),
                },
            }
        }
        bail!(
            "cache entry of shard {} kept changing during the read",
            s.object
        )
    }
    /// Copy from a pinned entry. `Ok(false)`: missing or failed verification.
    fn copy_cached(&self, s: &Shard, within: u64, n: usize, out: &mut Vec<u8>) -> Result<bool> {
        if !self.valid(s)? {
            return Ok(false);
        }
        let f = match OpenOptions::new().read(true).write(true).open(self.path(s)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let old = out.len();
        out.resize(old + n, 0);
        crate::utils::read_exact_at(&f, &mut out[old..], within)?;
        // Explicit access updates are independent of OS noatime/relatime.
        f.set_times(std::fs::FileTimes::new().set_accessed(SystemTime::now()))?;
        Ok(true)
    }
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
                    for (slot, block) in blocks.iter_mut().enumerate().take(2) {
                        if let Some(bytes) = payloads.get(group * 2 + slot) {
                            block[..bytes.len()].copy_from_slice(bytes);
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
        let cache = ShardCache::new(temp.path().into(), 0).unwrap();
        assert_eq!(cache.startup_removed_bytes, 8);
        assert_eq!(cache.cleanup().unwrap(), 0);
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
