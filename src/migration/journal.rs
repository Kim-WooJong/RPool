//! Cloud journal (work package B): the frozen plan and append-only progress
//! records of one migration, replicated to the pool's remotes under
//! `.rpool-sync/migrations-v1/<scope>/<migration_id>/`, encrypted like pool
//! sync metadata, with a local cache for offline status.
//!
//! Layout (per remote of the *saved* pool, after `apply_remote_roots`; the old
//! remotes of a changed pool are not written, because they may be leaving):
//!
//! ```text
//! <remote>/.rpool-sync/migrations-v1/<scope>/<migration_id>/plan.json
//! <remote>/.rpool-sync/migrations-v1/<scope>/<migration_id>/records/<blake3>.json
//! <remote>/.rpool-sync/migrations-v1/<scope>/<migration_id>/retire/<blake3>.json
//! ```
//!
//! `retire/` holds the cleanup records of phase 4 (`migration::retire`),
//! written and read with the same rules as `records/`.
//!
//! `<scope>` is the pool-sync scope (`blake3(json(("rpool-pool-sync-v6", pool)))`),
//! so the journal is a sibling of `events-v6/<scope>`. Every object goes through
//! the crypt remote (rclone crypt, or RPool's native crypt for `native_crypt`
//! pools, read back through rclone crypt), so names and contents are encrypted.
//!
//! Every object is write-once: an existing object is never overwritten or
//! deleted. Records are content-addressed by the blake3 of their JSON bytes.
//! A local mirror lives under `<config dir>/migrations/<pool>/<migration_id>/`.
//!
//! Failure semantics:
//! - `publish_plan` refuses when any readable copy (cloud or cache) holds a
//!   different plan; it succeeds when at least one cloud replica stored or
//!   already held the identical plan. Publishing again heals missed replicas.
//! - `append` succeeds when at least one cloud replica stored the record;
//!   failed replicas are reported as `[warning]` lines on stderr.
//! - `records` is the union of every readable replica and the cache. A record
//!   whose bytes do not match its name or do not parse is skipped with a
//!   `[warning]`. With every replica unreachable, the cache alone is used
//!   (with a warning) if this PC has seen the migration; otherwise it errors.
//! - `load_plan` returns the plan; it errors when readable copies disagree, or
//!   when nothing was found and no replica could be read.
//!
//! Replica I/O runs concurrently (one thread per store; there are only as many
//! stores as pool remotes), and results are merged in store order, so one slow
//! provider no longer serialises the others. `Journal` is `Send + Sync`: appends
//! of different records from several threads touch different objects.
use super::model::{Plan, Record};
use crate::prelude::*;
use crate::storage::{
    error::{StorageError, StorageErrorKind},
    rclone::RcloneContext,
    traits::OperationContext,
    writer::StorageWriter,
};
use crate::utils::remote_join;
use std::sync::Arc;

/// Object name of the frozen plan.
const PLAN: &str = "plan.json";
/// Directory of the progress records.
const RECORDS: &str = "records";
/// Cleanup (phase 4 `retire`) records: a sibling of `records/`, so the
/// migration fold and older binaries never see them.
pub(crate) const RETIRE: &str = "retire";
/// Tag hashed with the pool name into the pool-sync scope (shared with pool sync).
const SCOPE_TAG: &str = "rpool-pool-sync-v6";

/// Write-once storage for the journals of one pool (one replica, or the
/// local cache). Paths are relative to `<migration_id>/`.
pub(crate) trait JournalStore: Send + Sync {
    /// Short description for warnings (never contains secrets).
    fn label(&self) -> String;
    /// Migration ids present (directory names).
    fn list_migrations(&self) -> Result<Vec<String>>;
    /// `Ok(None)` when the object does not exist.
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>>;
    /// Creates the object. `Ok(None)` when written and verified, or
    /// `Ok(Some(existing))` when an object already exists (left untouched).
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>>;
    /// Ids (`<blake3>`) under `records/`; empty when absent.
    fn list_records(&self, migration: &str) -> Result<Vec<String>>;
    /// Ids (`<blake3>`) under `dir/` (`records` or [`RETIRE`]); empty when
    /// absent. Stores that only know `records/` refuse other directories.
    fn list_ids(&self, migration: &str, dir: &str) -> Result<Vec<String>> {
        if dir == RECORDS {
            return self.list_records(migration);
        }
        bail!("{} cannot list {dir}/", self.label())
    }
}

/// The cloud journal of one migration of one pool: replicated cloud stores
/// plus an optional local cache. Opened by `pool migrate` plan/run/status/adopt.
pub(crate) struct Journal {
    /// Pool name.
    pool: String,
    /// Migration id.
    migration_id: String,
    /// One store per pool remote; at least one.
    cloud: Vec<Arc<dyn JournalStore>>,
    /// Local mirror, if the config directory is available.
    cache: Option<Arc<dyn JournalStore>>,
}

impl Journal {
    /// Opens the journal of `migration_id` for `pool` (uses the pool's saved
    /// remotes; nothing is written until a publish/append).
    pub(crate) fn open(rclone: &str, pool: &str, migration_id: &str) -> Result<Self> {
        let (cloud, cache) = stores(rclone, pool)?;
        Self::with_stores(pool, migration_id, cloud, cache)
    }

    /// Journal over explicit stores (tests, e2e, other PCs' caches).
    pub(crate) fn with_stores(
        pool: &str,
        migration_id: &str,
        cloud: Vec<Arc<dyn JournalStore>>,
        cache: Option<Arc<dyn JournalStore>>,
    ) -> Result<Self> {
        crate::pool::validate_pool_name(pool)?;
        validate_migration_id(migration_id)?;
        if cloud.is_empty() {
            bail!("migration journal requires at least one pool remote");
        }
        Ok(Self {
            pool: pool.into(),
            migration_id: migration_id.into(),
            cloud,
            cache,
        })
    }

    /// Cloud stores followed by the cache, in that order.
    fn all(&self) -> Vec<&Arc<dyn JournalStore>> {
        self.cloud.iter().chain(self.cache.iter()).collect()
    }

    /// Writes the frozen plan (idempotent: the same plan may be published again).
    pub(crate) fn publish_plan(&self, plan: &Plan) -> Result<()> {
        if plan.migration_id != self.migration_id || plan.pool != self.pool {
            bail!("plan belongs to a different pool or migration id");
        }
        let bytes = serde_json::to_vec(plan)?;
        let want: Value = serde_json::from_slice(&bytes)?;
        let id = &self.migration_id;
        let same =
            |existing: &[u8]| serde_json::from_slice::<Value>(existing).ok() == Some(want.clone());
        // Refuse before writing anything when a readable copy already differs.
        let stores = self.all();
        let copies = concurrently(stores.clone(), |store| store.read(id, PLAN));
        for (store, copy) in stores.into_iter().zip(copies) {
            if let Ok(Some(existing)) = copy {
                if !same(&existing) {
                    bail!(
                        "a different plan already exists for migration {id} on {}; refusing to overwrite",
                        store.label()
                    );
                }
            }
        }
        let mut stored = 0usize;
        let mut failures = vec![];
        let created = concurrently(self.cloud.iter().collect(), |store| {
            store.create(id, PLAN, &bytes)
        });
        for (store, outcome) in self.cloud.iter().zip(created) {
            match outcome {
                Ok(None) => stored += 1,
                Ok(Some(existing)) if same(&existing) => stored += 1,
                Ok(Some(_)) => bail!(
                    "a different plan appeared for migration {id} on {}; refusing to overwrite",
                    store.label()
                ),
                Err(error) => failures.push(format!("{}: {error:#}", store.label())),
            }
        }
        if stored == 0 {
            bail!(
                "migration plan could not be stored on any pool remote: {}",
                failures.join("; ")
            );
        }
        warn_failures("plan not stored", &failures);
        if let Some(cache) = &self.cache {
            match cache.create(id, PLAN, &bytes) {
                Ok(Some(existing)) if !same(&existing) => {
                    bail!("the local cache holds a different plan for migration {id}")
                }
                Ok(_) => {}
                Err(error) => warn(&format!("local plan cache not written: {error:#}")),
            }
        }
        Ok(())
    }

    /// Reads the plan from every store. `Ok(None)` when no copy exists and some
    /// cloud replica was reachable; errors when copies disagree or nothing could
    /// be read.
    pub(crate) fn load_plan(&self) -> Result<Option<Plan>> {
        let id = &self.migration_id;
        let mut found: Option<(Value, Plan)> = None;
        let mut reachable = false;
        let mut failures = vec![];
        let mut cloud_bytes = None;
        let stores = self.all();
        let copies = concurrently(stores.clone(), |store| store.read(id, PLAN));
        for (index, (store, copy)) in stores.into_iter().zip(copies).enumerate() {
            let is_cache = index >= self.cloud.len();
            let bytes = match copy {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    // A missing cache entry says nothing about the cloud.
                    reachable |= !is_cache;
                    continue;
                }
                Err(error) => {
                    failures.push(format!("{}: {error:#}", store.label()));
                    continue;
                }
            };
            reachable = true;
            let parsed = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| {
                    Some((value.clone(), serde_json::from_value::<Plan>(value).ok()?))
                })
                .filter(|(_, plan)| plan.migration_id == *id && plan.pool == self.pool);
            let Some((value, plan)) = parsed else {
                warn(&format!(
                    "invalid plan for migration {id} on {} skipped",
                    store.label()
                ));
                continue;
            };
            if !is_cache && cloud_bytes.is_none() {
                cloud_bytes = Some(bytes);
            }
            match &found {
                Some((first, _)) if *first != value => bail!(
                    "plan replicas of migration {id} disagree ({} differs); refusing to continue",
                    store.label()
                ),
                Some(_) => {}
                None => found = Some((value, plan)),
            }
        }
        warn_failures("plan not read", &failures);
        if let (Some(cache), Some(bytes)) = (&self.cache, cloud_bytes) {
            if let Err(error) = cache.create(id, PLAN, &bytes) {
                warn(&format!("local plan cache not written: {error:#}"));
            }
        }
        match found {
            Some((_, plan)) => Ok(Some(plan)),
            None if reachable => Ok(None),
            None => bail!("migration journal unreachable: {}", failures.join("; ")),
        }
    }

    /// Appends one immutable record to every reachable replica.
    pub(crate) fn append(&self, record: &Record) -> Result<()> {
        self.append_in(RECORDS, record)
    }

    /// Appends one immutable, content-addressed object under `dir/` (same
    /// replication and failure semantics as [`Journal::append`]).
    pub(crate) fn append_in<T: Serialize>(&self, dir: &str, record: &T) -> Result<()> {
        let bytes = serde_json::to_vec(record)?;
        let rel = object_path(dir, &blake3::hash(&bytes).to_hex());
        let mut stored = 0usize;
        let mut failures = vec![];
        let created = concurrently(self.cloud.iter().collect(), |store| {
            store.create(&self.migration_id, &rel, &bytes)
        });
        for (store, outcome) in self.cloud.iter().zip(created) {
            match outcome {
                Ok(None) => stored += 1,
                Ok(Some(existing)) if existing == bytes => stored += 1,
                Ok(Some(_)) => failures.push(format!(
                    "{}: existing record differs from its content address",
                    store.label()
                )),
                Err(error) => failures.push(format!("{}: {error:#}", store.label())),
            }
        }
        if stored == 0 {
            bail!(
                "migration record could not be stored on any pool remote: {}",
                failures.join("; ")
            );
        }
        warn_failures("record not stored", &failures);
        if let Some(cache) = &self.cache {
            if let Err(error) = cache.create(&self.migration_id, &rel, &bytes) {
                warn(&format!("local record cache not written: {error:#}"));
            }
        }
        Ok(())
    }

    /// Creates a write-once side document of this migration (`rel` beside
    /// `plan.json`, e.g. the drive plan or the adoption marker) on every
    /// reachable replica and the cache. `Ok(None)` when this call's bytes
    /// are stored (or identical bytes already were); `Ok(Some(existing))`
    /// with the first different copy found, which is left untouched (callers
    /// compare by meaning). Errors when no replica stored or held a copy.
    pub(crate) fn create_document(&self, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_document(rel)?;
        let id = &self.migration_id;
        let mut held = 0usize;
        let mut existing = None;
        let mut failures = vec![];
        let created = concurrently(self.cloud.iter().collect(), |store| {
            store.create(id, rel, bytes)
        });
        for (store, outcome) in self.cloud.iter().zip(created) {
            match outcome {
                Ok(None) => held += 1,
                Ok(Some(copy)) => {
                    held += 1;
                    if copy != bytes && existing.is_none() {
                        existing = Some(copy);
                    }
                }
                Err(error) => failures.push(format!("{}: {error:#}", store.label())),
            }
        }
        if held == 0 {
            bail!(
                "{rel} could not be stored on any pool remote: {}",
                failures.join("; ")
            );
        }
        warn_failures(&format!("{rel} not stored"), &failures);
        if let Some(cache) = &self.cache {
            let cached = existing.as_deref().unwrap_or(bytes);
            if let Err(error) = cache.create(id, rel, cached) {
                warn(&format!("local {rel} cache not written: {error:#}"));
            }
        }
        Ok(existing)
    }

    /// Every distinct readable copy of a side document: cloud replicas first,
    /// then the cache. A copy found in the cloud is cached. Empty when no
    /// replica has it; an error only when nothing was found and no cloud
    /// replica could be read. `cache_first`: return the cached copy without
    /// asking the cloud (for write-once markers already seen here).
    pub(crate) fn read_documents(&self, rel: &str, cache_first: bool) -> Result<Vec<Vec<u8>>> {
        validate_document(rel)?;
        let id = &self.migration_id;
        if cache_first {
            if let Some(cache) = &self.cache {
                if let Ok(Some(bytes)) = cache.read(id, rel) {
                    return Ok(vec![bytes]);
                }
            }
        }
        let mut copies: Vec<Vec<u8>> = vec![];
        let mut reachable = false;
        let mut failures = vec![];
        let reads = concurrently(self.cloud.iter().collect(), |store| store.read(id, rel));
        for (store, read) in self.cloud.iter().zip(reads) {
            match read {
                Ok(Some(bytes)) => {
                    reachable = true;
                    if !copies.contains(&bytes) {
                        copies.push(bytes);
                    }
                }
                Ok(None) => reachable = true,
                Err(error) => failures.push(format!("{}: {error:#}", store.label())),
            }
        }
        if let Some(cache) = &self.cache {
            if let Some(first) = copies.first() {
                if let Err(error) = cache.create(id, rel, first) {
                    warn(&format!("local {rel} cache not written: {error:#}"));
                }
            }
            if let Ok(Some(bytes)) = cache.read(id, rel) {
                if !copies.contains(&bytes) {
                    copies.push(bytes);
                }
            }
        }
        if copies.is_empty() && !reachable {
            bail!("migration journal unreachable: {}", failures.join("; "));
        }
        warn_failures(&format!("{rel} not read"), &failures);
        Ok(copies)
    }

    /// Id of this migration.
    pub(crate) fn migration_id(&self) -> &str {
        &self.migration_id
    }

    /// All records from all reachable replicas (duplicates allowed).
    pub(crate) fn records(&self) -> Result<Vec<Record>> {
        self.records_in(RECORDS)
    }

    /// Every object under `dir/` from all reachable replicas and the cache
    /// (same union and failure semantics as [`Journal::records`]).
    pub(crate) fn records_in<T: serde::de::DeserializeOwned>(&self, dir: &str) -> Result<Vec<T>> {
        let id = &self.migration_id;
        let record_path = |rid: &str| object_path(dir, rid);
        let mut out: BTreeMap<String, T> = BTreeMap::new();
        // Cached records are immutable and content-addressed: never re-downloaded.
        let mut seen_locally = false;
        if let Some(cache) = &self.cache {
            seen_locally = matches!(cache.read(id, PLAN), Ok(Some(_)));
            match cache.list_ids(id, dir) {
                Ok(ids) => {
                    seen_locally |= !ids.is_empty();
                    for rid in ids {
                        match cache.read(id, &record_path(&rid)) {
                            Ok(Some(bytes)) => {
                                if let Some(record) = decode_record(&rid, &bytes, cache.label()) {
                                    out.insert(rid, record);
                                }
                            }
                            Ok(None) => {}
                            Err(error) => warn(&format!("cached record unreadable: {error:#}")),
                        }
                    }
                }
                Err(error) => warn(&format!("local record cache unreadable: {error:#}")),
            }
        }
        let mut reachable = 0usize;
        let mut failures = vec![];
        let listed = concurrently(self.cloud.iter().collect(), |store| store.list_ids(id, dir));
        // Missing record id -> indexes (store order) of the stores listing it.
        let mut holders: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, (store, ids)) in self.cloud.iter().zip(listed).enumerate() {
            let ids = match ids {
                Ok(ids) => ids,
                Err(error) => {
                    failures.push(format!("{}: {error:#}", store.label()));
                    continue;
                }
            };
            reachable += 1;
            for rid in ids {
                if !out.contains_key(&rid) {
                    let stores = holders.entry(rid).or_default();
                    if stores.last() != Some(&index) {
                        stores.push(index);
                    }
                }
            }
        }
        // Each missing record is downloaded once, from the least-loaded store
        // listing it, all stores at once; a missing, failed or invalid copy is
        // retried on the next store listing it (records are content-addressed,
        // so any valid copy is the same record).
        while !holders.is_empty() {
            let mut batches: Vec<Vec<String>> = vec![vec![]; self.cloud.len()];
            for (rid, stores) in &mut holders {
                let pick = (0..stores.len())
                    .min_by_key(|&p| batches[stores[p]].len())
                    .unwrap_or_default();
                batches[stores.remove(pick)].push(rid.clone());
            }
            let work: Vec<_> = self.cloud.iter().zip(batches).collect();
            let reads = concurrently(work, |(store, rids)| {
                rids.into_iter()
                    .map(|rid| {
                        let read = store.read(id, &record_path(&rid));
                        (rid, read)
                    })
                    .collect::<Vec<_>>()
            });
            for (store, results) in self.cloud.iter().zip(reads) {
                for (rid, read) in results {
                    let bytes = match read {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => continue,
                        Err(error) => {
                            failures.push(format!("{} record {rid}: {error:#}", store.label()));
                            continue;
                        }
                    };
                    let Some(record) = decode_record(&rid, &bytes, store.label()) else {
                        continue;
                    };
                    if let Some(cache) = &self.cache {
                        if let Err(error) = cache.create(id, &record_path(&rid), &bytes) {
                            warn(&format!("local record cache not written: {error:#}"));
                        }
                    }
                    holders.remove(&rid);
                    out.insert(rid, record);
                }
            }
            holders.retain(|_, stores| !stores.is_empty());
        }
        if reachable == 0 {
            if !seen_locally {
                bail!(
                    "migration journal unreachable and not cached locally: {}",
                    failures.join("; ")
                );
            }
            warn("every pool remote is unreachable; using the local cache only");
        }
        warn_failures("records not read", &failures);
        Ok(out.into_values().collect())
    }
}

/// Migration ids recorded in the cloud for `pool`, newest first.
pub(crate) fn discover(rclone: &str, pool: &str) -> Result<Vec<String>> {
    let (cloud, cache) = stores(rclone, pool)?;
    discover_in(pool, cloud, cache)
}

/// `discover` over explicit stores.
pub(crate) fn discover_in(
    pool: &str,
    cloud: Vec<Arc<dyn JournalStore>>,
    cache: Option<Arc<dyn JournalStore>>,
) -> Result<Vec<String>> {
    crate::pool::validate_pool_name(pool)?;
    let mut ids = BTreeSet::new();
    let mut reachable = 0usize;
    let mut failures = vec![];
    let stores: Vec<_> = cloud.iter().chain(cache.iter()).collect();
    let listed = concurrently(stores.clone(), |store| store.list_migrations());
    for (store, found) in stores.into_iter().zip(listed) {
        match found {
            Ok(found) => {
                reachable += 1;
                ids.extend(
                    found
                        .into_iter()
                        .filter(|id| validate_migration_id(id).is_ok()),
                );
            }
            Err(error) => failures.push(format!("{}: {error:#}", store.label())),
        }
    }
    if reachable == 0 {
        bail!("migration journal unreachable: {}", failures.join("; "));
    }
    warn_failures("migrations not listed", &failures);
    let mut dated = vec![];
    for id in ids {
        let journal = Journal::with_stores(pool, &id, cloud.clone(), cache.clone())?;
        match journal.load_plan() {
            Ok(Some(plan)) => dated.push((plan.created_unix, id)),
            Ok(None) => warn(&format!("migration {id} has no readable plan; skipped")),
            Err(error) => warn(&format!("migration {id} skipped: {error:#}")),
        }
    }
    dated.sort_by(|a, b| b.cmp(a));
    Ok(dated.into_iter().map(|(_, id)| id).collect())
}

/// Journal roots of `pool` on the remotes of `policy`.
pub(crate) fn roots(pool: &str, policy: &PoolDefinition) -> Result<Vec<String>> {
    crate::pool::validate_pool_name(pool)?;
    let scope = blake3::hash(&serde_json::to_vec(&(SCOPE_TAG, pool))?)
        .to_hex()
        .to_string();
    let mut roots = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        bail!("migration journal requires at least one pool remote");
    }
    Ok(roots
        .iter()
        .map(|root| remote_join(root, &format!(".rpool-sync/migrations-v1/{scope}")))
        .collect())
}

/// Cloud replicas and the optional local cache.
pub(crate) type Stores = (Vec<Arc<dyn JournalStore>>, Option<Arc<dyn JournalStore>>);

/// Cloud stores for the saved policy of `pool` plus the local cache
/// (`<config>/migrations/<pool>`); a missing config dir only warns.
fn stores(rclone: &str, pool: &str) -> Result<Stores> {
    crate::pool::validate_pool_name(pool)?;
    let store = crate::pool::load_pool_store()?;
    let policy = store
        .pools
        .get(pool)
        .with_context(|| format!("pool {pool} is not configured"))?;
    let cache =
        match crate::config::app_config_dir() {
            Ok(dir) => Some(Arc::new(LocalStore::new(dir.join("migrations").join(pool)))
                as Arc<dyn JournalStore>),
            Err(error) => {
                warn(&format!("no local migration cache: {error:#}"));
                None
            }
        };
    Ok((cloud_stores(rclone, pool, policy)?, cache))
}

/// One cloud store per remote of `policy`.
pub(crate) fn cloud_stores(
    rclone: &str,
    pool: &str,
    policy: &PoolDefinition,
) -> Result<Vec<Arc<dyn JournalStore>>> {
    roots(pool, policy)?
        .into_iter()
        .map(|root| {
            Ok(
                Arc::new(CloudStore::new(rclone, &root, policy.native_crypt)?)
                    as Arc<dyn JournalStore>,
            )
        })
        .collect()
}

/// Accepts 1–128 ASCII letters, digits, `-`, `_`, `.`, not starting with a dot.
pub(crate) fn validate_migration_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || id.starts_with('.')
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        bail!("invalid migration id");
    }
    Ok(())
}

/// Side documents are single file names beside `plan.json`.
fn validate_document(rel: &str) -> Result<()> {
    if rel == PLAN
        || !rel.ends_with(".json")
        || rel.starts_with('.')
        || !rel
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        bail!("invalid migration document name: {rel}");
    }
    Ok(())
}

/// The cloud replicas and the local cache of `pool`'s migration journals
/// (`Journal::with_stores` / `discover_in` inputs), for readers that look at
/// many migrations at once.
pub(crate) fn pool_stores(rclone: &str, pool: &str) -> Result<Stores> {
    stores(rclone, pool)
}

/// Migration ids listed by any reachable store (no plan is read). Errors
/// only when no store could be listed.
pub(crate) fn list_ids(
    cloud: &[Arc<dyn JournalStore>],
    cache: Option<&Arc<dyn JournalStore>>,
) -> Result<BTreeSet<String>> {
    let stores: Vec<_> = cloud.iter().chain(cache).collect();
    let listed = concurrently(stores.clone(), |store| store.list_migrations());
    let mut ids = BTreeSet::new();
    let mut reachable = 0usize;
    let mut failures = vec![];
    for (store, found) in stores.into_iter().zip(listed) {
        match found {
            Ok(found) => {
                reachable += 1;
                ids.extend(
                    found
                        .into_iter()
                        .filter(|id| validate_migration_id(id).is_ok()),
                );
            }
            Err(error) => failures.push(format!("{}: {error:#}", store.label())),
        }
    }
    if reachable == 0 {
        bail!("migration journal unreachable: {}", failures.join("; "));
    }
    Ok(ids)
}

/// Relative path of record `id` in `dir`.
fn object_path(dir: &str, id: &str) -> String {
    format!("{dir}/{id}.json")
}

#[cfg(test)]
fn record_path(id: &str) -> String {
    object_path(RECORDS, id)
}

/// A record id is a 64-digit lowercase hex BLAKE3.
fn valid_record_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Parses a record, skipping (with a warning) bytes that do not match their
/// content address or do not parse.
fn decode_record<T: serde::de::DeserializeOwned>(
    id: &str,
    bytes: &[u8],
    label: String,
) -> Option<T> {
    if blake3::hash(bytes).to_hex().as_str() != id {
        warn(&format!(
            "record {id} on {label} does not match its content address; skipped"
        ));
        return None;
    }
    match serde_json::from_slice(bytes) {
        Ok(record) => Some(record),
        Err(error) => {
            warn(&format!(
                "record {id} on {label} is not a valid record ({error}); skipped"
            ));
            None
        }
    }
}

/// Runs `f` on every item at once (one scoped thread each; callers pass one
/// item per store, and a pool has only a few remotes). Results keep the input
/// order, so merges and failure lists stay deterministic. A panic in `f` is
/// re-raised on the caller's thread.
fn concurrently<I: Send, T: Send>(items: Vec<I>, f: impl Fn(I) -> T + Sync) -> Vec<T> {
    if items.len() <= 1 {
        return items.into_iter().map(f).collect();
    }
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .into_iter()
            .map(|item| scope.spawn(move || f(item)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

// `execute.rs` appends records of different archives from several threads.
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<Journal>();
};

/// Prints a `[warning]` line about the journal to stderr.
fn warn(message: &str) {
    eprintln!("[warning] migration journal: {message}");
}

/// Prints one warning per failed replica.
fn warn_failures(what: &str, failures: &[String]) {
    for failure in failures {
        warn(&format!("{what}: {failure}"));
    }
}

/// Whether `error` is a storage "not found" error.
fn not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<StorageError>()
        .is_some_and(|e| e.kind() == StorageErrorKind::NotFound)
}

/// One replica: `<scope root>` on a crypt remote.
pub(crate) struct CloudStore {
    /// rclone executable.
    rclone: String,
    /// Journal scope root, `remote:path` without a trailing slash.
    root: String,
    /// The pool uses RPool's native crypt for writes.
    native_crypt: bool,
}

/// One entry of `rclone lsjson` output.
#[derive(Deserialize)]
struct Listed {
    /// Path relative to the listed directory.
    #[serde(rename = "Path")]
    path: String,
    /// Entry is a directory.
    #[serde(rename = "IsDir")]
    is_dir: bool,
}

impl CloudStore {
    /// Validates `root` (`remote:path`, no `..`, control chars or extra colons).
    pub(crate) fn new(rclone: &str, root: &str, native_crypt: bool) -> Result<Self> {
        // Same syntax rules as the pool-sync roots.
        let Some((remote, path)) = root.split_once(':') else {
            bail!("journal root must be remote:path");
        };
        if remote.is_empty()
            || remote.starts_with('-')
            || !remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-. ".contains(&b))
            || path.contains(':')
            || path.contains('\\')
            || root.chars().any(char::is_control)
            || path.split('/').any(|p| p == "." || p == "..")
        {
            bail!("invalid journal root");
        }
        Ok(Self {
            rclone: rclone.into(),
            root: root.trim_end_matches('/').into(),
            native_crypt,
        })
    }
    /// Writer for this pool's crypt mode.
    fn writer(&self) -> StorageWriter {
        StorageWriter::for_pool(&self.rclone, self.native_crypt)
    }
    /// Full address of `rel` within `migration`.
    fn address(&self, migration: &str, rel: &str) -> String {
        remote_join(&self.root, &format!("{migration}/{rel}"))
    }
    /// Lists `dir` through the crypt remote (decrypted names).
    fn list(
        &self,
        dir: &str,
        dirs: bool,
        filter: Option<&str>,
    ) -> Result<Option<Vec<Listed>>, StorageError> {
        let context = RcloneContext::inherited(&self.rclone);
        let operation = OperationContext::none();
        context.ensure_crypt(&operation, &self.root)?;
        let mut args = vec!["lsjson", if dirs { "--dirs-only" } else { "--files-only" }];
        if let Some(filter) = filter {
            args.extend(["--include", filter]);
        }
        args.extend(["--", dir]);
        match context.capture(&operation, &args) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| StorageError::invalid_input("invalid rclone listing")),
            Err(error) if error.kind() == StorageErrorKind::NotFound => Ok(Some(vec![])),
            Err(StorageError::OutputBoundsViolated) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl JournalStore for CloudStore {
    fn label(&self) -> String {
        self.root
            .split_once(':')
            .map_or_else(|| self.root.clone(), |(remote, _)| format!("{remote}:"))
    }
    fn list_migrations(&self) -> Result<Vec<String>> {
        let listed = self
            .list(&self.root, true, None)?
            .context("migration listing exceeds the output bound")?;
        Ok(listed
            .into_iter()
            .filter(|e| e.is_dir)
            .map(|e| e.path)
            .collect())
    }
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        let address = self.address(migration, rel);
        match self.writer().reader().read_metadata(&address) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        let storage = self.writer();
        storage.ensure_destination(&self.root)?;
        let address = self.address(migration, rel);
        match storage.reader().stat(&address) {
            Ok(_) => return Ok(Some(storage.reader().read_metadata(&address)?)),
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(error),
        }
        // Cooperating writers publish identical bytes at one address; rclone has
        // no atomic create-if-absent (same caveat as pool-sync events).
        // No second readback: `write_bytes` -> `write_file` returns `Ok` only
        // after `reader.verify(shard, true)` proved the stored object's size and
        // blake3 (either the pre-write `reusable` check or the post-write
        // readback), on both the native and the rclone route, and always read
        // through rclone crypt, the same path `read`/`read_metadata` use.
        storage.write_bytes(&address, bytes, 1)?;
        Ok(None)
    }
    fn list_records(&self, migration: &str) -> Result<Vec<String>> {
        self.list_ids(migration, RECORDS)
    }
    fn list_ids(&self, migration: &str, dir: &str) -> Result<Vec<String>> {
        let dir = self.address(migration, dir);
        let pages: Vec<Vec<Listed>> = match self.list(&dir, false, None)? {
            Some(all) => vec![all],
            // Past the 8 MiB listing bound: 16 hash-prefix pages, like pool sync.
            None => b"0123456789abcdef"
                .iter()
                .map(|p| {
                    let filter = format!("{}*.json", *p as char);
                    self.list(&dir, false, Some(&filter))?
                        .context("record listing page exceeds the output bound")
                })
                .collect::<Result<_>>()?,
        };
        Ok(pages
            .into_iter()
            .flatten()
            .filter(|e| !e.is_dir)
            .filter_map(|e| e.path.strip_suffix(".json").map(str::to_owned))
            .filter(|id| valid_record_id(id))
            .collect())
    }
}

/// Local mirror: `<config dir>/migrations/<pool>/`.
pub(crate) struct LocalStore {
    /// Directory holding one subdirectory per migration id.
    root: PathBuf,
}

impl LocalStore {
    /// Mirror rooted at `root`.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// Creates `path` recursively, owner-only (0700) on Unix.
fn private_dirs(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

impl JournalStore for LocalStore {
    fn label(&self) -> String {
        "local cache".into()
    }
    fn list_migrations(&self) -> Result<Vec<String>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut ids = vec![];
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_owned());
                }
            }
        }
        Ok(ids)
    }
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        match fs::read(self.root.join(migration).join(rel)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        let path = self.root.join(migration).join(rel);
        let parent = path.parent().context("cache parent missing")?;
        private_dirs(parent)?;
        if let Some(existing) = self.read(migration, rel)? {
            return Ok(Some(existing));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(Some(fs::read(&path)?));
            }
            Err(error) => return Err(error.error.into()),
        }
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(None)
    }
    fn list_records(&self, migration: &str) -> Result<Vec<String>> {
        self.list_ids(migration, RECORDS)
    }
    fn list_ids(&self, migration: &str, dir: &str) -> Result<Vec<String>> {
        let entries = match fs::read_dir(self.root.join(migration).join(dir)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut ids = vec![];
        for entry in entries {
            let name = entry?.file_name();
            if let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) {
                if valid_record_id(id) {
                    ids.push(id.to_owned());
                }
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
