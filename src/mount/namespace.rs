//! Metadata-only namespace. Cache absence is never interpreted as deletion.
//!
//! Holds `Namespace` (the workspace's events, pending write intents and
//! directories), its durable checkpoint + journal persistence (`load`/`save`),
//! and the shared helpers `durable_json`/`durable_bytes`, `random_id` and
//! `valid_path` used across `mount`.
use super::shared_model::{self, Content, Event, Resolved};
use crate::prelude::*;
pub(crate) use shared::Shared;

mod journal;
mod shared;
mod validation;

/// A local write or deletion recorded in the namespace but not yet
/// committed as an event. Created by `VirtualDrive` writes/deletes, persisted
/// as `spool/<id>/intent.json`, and committed by sync via [`Namespace::commit`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Intent {
    /// Random 64-hex identity (also the spool directory name for writes).
    pub id: String,
    /// Visible drive path the intent applies to.
    pub path: String,
    /// Path the committed event is recorded under: the parent revision's event
    /// path (continuing that file's history), else `path` itself.
    pub event_path: String,
    /// Event ids this intent descends from (the ancestry captured at edit time).
    pub parents: Vec<String>,
    /// Spool image id for a write (equals `id`); `None` for a deletion.
    pub spool: Option<String>,
    /// Sealed size of the spool image in bytes (0 until sealed).
    pub size: u64,
    /// Hex BLAKE3 of the sealed spool image (empty until sealed).
    pub hash: String,
    /// Earlier local intent this one must follow; its committed event becomes
    /// the parent instead of `parents`.
    pub depends_on: Option<String>,
}
/// The workspace namespace. `version` is 6 (pool sync) for every opened
/// workspace; [`Namespace::create`] starts at 3 (local-only resolution),
/// which only the test fixture keeps.
///
/// The large collections are [`Shared`]: a clone is O(1) and a mutation
/// copies only the collections it writes. [`Namespace::save`] appends a delta
/// to the journal (see [`journal`]) and rewrites the full checkpoint only
/// when the journal has grown to the checkpoint's size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Namespace {
    /// Format version: 6 (pool sync) or 3 (local-only test fixture).
    pub version: u32,
    /// Save counter, incremented by every `save`; journal replay needs
    /// consecutive generations.
    pub generation: u64,
    /// Random identity of this workspace's device, written into its events.
    pub device: String,
    /// Worker (PC) name, written into events; reset to the caller's name on load.
    pub worker: String,
    /// Every known event by content id (local and pulled from the pool).
    pub events: Shared<BTreeMap<String, Event>>,
    /// Event ids already published to the pool-sync metadata.
    pub published: Shared<BTreeSet<String>>,
    /// Uncommitted local intents, in commit order.
    pub pending: Vec<Intent>,
    /// Committed intent id -> event id it produced (commit receipts).
    pub committed_intents: Shared<BTreeMap<String, String>>,
    /// Explicitly created (possibly empty) directories.
    pub directories: Shared<BTreeSet<String>>,
    /// Conservative edit ancestry: listing never advances a writer's baseline.
    pub bases: Shared<BTreeMap<String, Vec<String>>>,
    /// What is on disk for this namespace, if it was loaded or saved.
    #[serde(skip)]
    durable: Option<Arc<Durable>>,
    /// Cached projection of `events` (see [`Namespace::projected`]).
    #[serde(skip)]
    projection: ProjectionCache,
    /// Cached validation results keyed by the collections they checked.
    #[serde(skip)]
    checks: validation::CheckCache,
}

/// The persisted state a save extends: the checkpoint, the journal's valid
/// end, and the namespace they hold (sharing collections with the live one).
struct Durable {
    /// Workspace directory holding the files.
    dir: PathBuf,
    /// Hash of the checkpoint (`namespace.json`) the journal extends.
    checkpoint: String,
    /// Checkpoint file size in bytes; bounds the journal before compaction.
    checkpoint_len: u64,
    /// Valid end of the journal (length and record count).
    tail: journal::Tail,
    /// The namespace as persisted, the base for the next delta.
    saved: Namespace,
}

impl std::fmt::Debug for Durable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Durable")
            .field("checkpoint", &self.checkpoint)
            .field("tail", &self.tail)
            .finish_non_exhaustive()
    }
}

/// Shared resolved view: visible path -> resolved revision.
pub(crate) type Projected = Arc<BTreeMap<String, Resolved>>;

/// [`Namespace::resolved`] of one exact event map (see [`Shared::is`]).
#[derive(Default)]
struct ProjectionCache(Mutex<Option<ProjectionKeyed>>);
/// Cache key and value: the event map it was computed from (weak), the
/// namespace version, and the projection.
type ProjectionKeyed = (std::sync::Weak<BTreeMap<String, Event>>, u32, Projected);

impl Clone for ProjectionCache {
    fn clone(&self) -> Self {
        Self(Mutex::new(
            self.0.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        ))
    }
}

impl std::fmt::Debug for ProjectionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProjectionCache")
    }
}

/// Journaled envelope: old binaries (which read `hash`) refuse it instead
/// of loading the checkpoint without its journal.
const ENVELOPE_V2: &[u8] = br#"{"format":2,"blake3":""#;
/// The envelope every earlier version wrote; still loaded.
const ENVELOPE_V1: &[u8] = br#"{"hash":""#;
/// Key between the hash and the payload in a canonical envelope.
const PAYLOAD_KEY: &[u8] = br#"","payload":"#;
/// The journal size at which a save compacts instead: the checkpoint's size,
/// so compaction rewrites at most as many bytes as the records it folds.
fn journal_limit(checkpoint_len: u64) -> u64 {
    // Tests compact small namespaces often so both save paths stay exercised.
    #[cfg(test)]
    return checkpoint_len;
    #[cfg(not(test))]
    checkpoint_len.max(256 * 1024)
}
/// Bounds replay work at load.
const MAX_JOURNAL_RECORDS: u64 = 20_000;

/// The original (v1) checkpoint layout, parsed when bytes are not canonical.
#[derive(Serialize, Deserialize)]
struct Envelope {
    /// Hex BLAKE3 of the serialized payload.
    hash: String,
    /// The namespace itself.
    payload: Namespace,
}

/// The payload bytes and hash of a canonical envelope (either version).
fn split_envelope(bytes: &[u8]) -> Option<(&[u8], &str)> {
    let prefix = [ENVELOPE_V2, ENVELOPE_V1]
        .into_iter()
        .find(|p| bytes.starts_with(p))?;
    let rest = &bytes[prefix.len()..];
    let hash = std::str::from_utf8(rest.get(..64)?).ok()?;
    let payload = rest
        .get(64..)?
        .strip_prefix(PAYLOAD_KEY)?
        .strip_suffix(b"}")?;
    Some((payload, hash))
}

/// The hash of a verified envelope and, if `parse`, its namespace.
fn open_envelope(bytes: &[u8], parse: bool) -> Result<(String, Option<Namespace>)> {
    if let Some((payload, hash)) = split_envelope(bytes) {
        if blake3::hash(payload).to_hex().as_str() != hash {
            bail!("namespace checksum mismatch");
        }
        let state = parse.then(|| serde_json::from_slice(payload)).transpose()?;
        return Ok((hash.into(), state));
    }
    // A non-canonical layout of the original envelope: verify the
    // re-serialized payload as earlier versions did.
    let envelope: Envelope = serde_json::from_slice(bytes)?;
    if blake3::hash(&serde_json::to_vec(&envelope.payload)?)
        .to_hex()
        .as_str()
        != envelope.hash
    {
        bail!("namespace checksum mismatch");
    }
    Ok((envelope.hash, Some(envelope.payload)))
}

/// The hash the checkpoint in `dir` declares (not verified), read from its
/// first bytes.
fn checkpoint_identity(dir: &Path) -> Result<Option<String>> {
    let mut head = vec![0u8; ENVELOPE_V2.len() + 64];
    let mut file = match File::open(dir.join("namespace.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut read = 0;
    while read < head.len() {
        match std::io::Read::read(&mut file, &mut head[read..])? {
            0 => break,
            n => read += n,
        }
    }
    head.truncate(read);
    Ok(head
        .strip_prefix(ENVELOPE_V2)
        .and_then(|h| std::str::from_utf8(h).ok())
        .filter(|h| h.len() == 64)
        .map(String::from))
}

/// New random 64-hex id (BLAKE3 of 24 random bytes). Used for intent,
/// epoch, worker and temporary-directory names.
pub(crate) fn random_id() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random identity: {e}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
/// Checks that `path` is a valid relative namespace path (no leading `/`,
/// no `\`, plus the portable event path rules). Used by rename, history and
/// validation.
pub(crate) fn valid_path(path: &str) -> Result<()> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        bail!("invalid namespace path");
    }
    // Reuse portable event validation without creating any remote object.
    Event {
        version: 1,
        worker: "validator".into(),
        device: "validator".into(),
        path: path.into(),
        parents: vec![],
        content: None,
    }
    .validate()
}
/// Writes `value` as JSON to `path` crash-safely via [`durable_bytes`]. Used
/// for intents, upload plans, caches and receipts.
pub(crate) fn durable_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    // Serialize in memory: `to_writer` on the unbuffered temporary issued one
    // write call per JSON token (seconds for a multi-megabyte namespace).
    durable_bytes(path, &serde_json::to_vec(value)?)
}
/// Removes `path` and flushes its directory (Unix); already absent is
/// success. Used to drop stale journals.
fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            #[cfg(unix)]
            File::open(path.parent().context("state parent missing")?)?.sync_all()?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}
/// Writes `bytes` to `path` crash-safely: temporary, flush, crash point,
/// atomic replace, directory flush.
pub(crate) fn durable_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("state parent missing")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("flush temporary for {}", path.display()))?;
    super::crash::point("durable.before_persist")?;
    crate::utils::persist_replacing(temp, path)?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}
/// The visible files of `events` as a namespace of `version` resolves them
/// (see [`Namespace::resolved`]); also used on a copy outside the drive lock.
pub(crate) fn resolve_events(
    version: u32,
    events: &BTreeMap<String, Event>,
) -> Result<BTreeMap<String, Resolved>> {
    if version == 6 {
        Ok(super::peer_projection::project(events)?.files)
    } else {
        shared_model::reduce(events)
    }
}
impl Namespace {
    /// Empty version-3 namespace with a fresh device id for `worker`; `load`
    /// creates and saves one when `dir` has no checkpoint.
    pub(crate) fn create(worker: &str) -> Result<Self> {
        valid_path(worker)?;
        Ok(Self {
            version: 3,
            generation: 0,
            device: random_id()?,
            worker: worker.into(),
            events: Shared::default(),
            published: Shared::default(),
            pending: vec![],
            committed_intents: Shared::default(),
            directories: Shared::default(),
            bases: Shared::default(),
            durable: None,
            projection: ProjectionCache::default(),
            checks: Default::default(),
        })
    }
    /// Checks the invariants `load` and `save` rely on: version, paths, event
    /// references, intent identities and dependencies, and no file/directory
    /// path conflicts among committed files plus pending writes.
    pub(crate) fn validate(&self) -> Result<()> {
        if !matches!(self.version, 3 | 6) {
            bail!("unsupported virtual namespace version");
        }
        valid_path(&self.worker)?;
        // Event validation: `resolved` reduces (and validates) the events
        // first; one projection serves both checks.
        let resolved = self.resolved_shared()?;
        self.checks.references(
            &self.events,
            &self.published,
            &self.bases,
            &self.committed_intents,
            || {
                if self
                    .published
                    .iter()
                    .any(|id| !self.events.contains_key(id))
                {
                    bail!("unknown published revision");
                }
                for (path, parents) in &*self.bases {
                    valid_path(path)?;
                    if parents.iter().any(|id| !self.events.contains_key(id)) {
                        bail!("missing observed ancestry");
                    }
                }
                for (id, event) in &*self.committed_intents {
                    if id.len() != 64
                        || !id.bytes().all(|b| b.is_ascii_hexdigit())
                        || !self.events.contains_key(event)
                    {
                        bail!("invalid committed intent reference");
                    }
                }
                Ok(())
            },
        )?;
        let mut ids = BTreeSet::new();
        for intent in &self.pending {
            valid_path(&intent.path)?;
            valid_path(&intent.event_path)?;
            if intent.id.len() != 64
                || !intent
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                bail!("invalid intent identity");
            }
            if let Some(previous) = &intent.depends_on {
                if !ids.contains(previous) && !self.committed_intents.contains_key(previous) {
                    bail!("missing or forward intent dependency");
                }
            }
            if !ids.insert(intent.id.clone()) {
                bail!("duplicate pending intent");
            }
            if let Some(spool) = &intent.spool {
                if spool != &intent.id {
                    bail!("invalid spool identity");
                }
            }
            for parent in &intent.parents {
                if self
                    .events
                    .get(parent)
                    .is_none_or(|event| event.path != intent.event_path)
                {
                    bail!("missing or cross-path intent ancestry");
                }
            }
        }
        let added: BTreeSet<&str> = self
            .pending
            .iter()
            .filter(|i| i.spool.is_some())
            .map(|i| i.path.as_str())
            .collect();
        if self
            .checks
            .index(&resolved, &self.directories)
            .is_some_and(|index| index.admits(&added))
        {
            return Ok(());
        }
        let mut live: BTreeSet<&str> = validation::committed_files(&resolved).collect();
        for intent in &self.pending {
            if intent.spool.is_some() {
                live.insert(&intent.path);
            } else {
                live.remove(intent.path.as_str());
            }
        }
        validation::check_paths(&live, &self.directories)
    }
    /// Opens the checkpoint in `dir` with its journal replayed. Never falls
    /// back to `namespace.previous.json` (+ `namespace.previous.journal`):
    /// that generation can omit acknowledged writes, so it is only reported.
    pub(crate) fn load(dir: &Path, worker: &str) -> Result<Self> {
        let primary = dir.join("namespace.json");
        if !primary.exists() && !dir.join("namespace.previous.json").exists() {
            let mut fresh = Self::create(worker)?;
            fresh.save(dir)?;
            return Ok(fresh);
        }
        let error = match Self::load_generation(dir, "namespace.json", journal::JOURNAL) {
            Ok(mut state) => {
                state.worker = worker.into();
                return Ok(state);
            }
            Err(error) => error,
        };
        let previous = dir.join("namespace.previous.json");
        if Self::load_generation(dir, "namespace.previous.json", journal::PREVIOUS_JOURNAL).is_ok()
        {
            // A previous generation can omit acknowledged writes. Do not
            // auto-publish or delete anything while the primary is corrupt.
            bail!("primary namespace corrupt ({error:#}); validated previous generation exists at {}. Preserve spool and restore explicitly",previous.display());
        }
        bail!("no valid namespace checkpoint ({error:#}); preserve spool and cache")
    }
    /// A verified checkpoint plus the valid records of its journal.
    fn load_generation(dir: &Path, checkpoint: &str, journal_file: &str) -> Result<Self> {
        let bytes = fs::read(dir.join(checkpoint))?;
        let (hash, state) = open_envelope(&bytes, true)?;
        let mut state = state.context("namespace payload missing")?;
        let tail = journal::replay(&dir.join(journal_file), &hash, &mut state)?;
        state.validate()?;
        state.durable = Some(Arc::new(Durable {
            dir: dir.to_path_buf(),
            checkpoint: hash,
            checkpoint_len: bytes.len() as u64,
            tail,
            saved: state.snapshot(),
        }));
        Ok(state)
    }
    /// The persisted fields, sharing this namespace's collections.
    fn snapshot(&self) -> Self {
        Self {
            durable: None,
            projection: ProjectionCache::default(),
            checks: Default::default(),
            ..self.clone()
        }
    }
    /// Validates and durably records this namespace (the next generation)
    /// in `dir`: normally one fsynced journal record holding the change
    /// since the last load or save; a full checkpoint when there is no such
    /// base, nothing changed (so the previous generation advances to this
    /// state, as before the journal), the journal outgrew the checkpoint, or
    /// another writer changed the files since.
    pub(crate) fn save(&mut self, dir: &Path) -> Result<()> {
        self.validate()?;
        self.generation = self
            .generation
            .checked_add(1)
            .context("namespace generation overflow")?;
        if let Some(base) = self.durable.clone().filter(|b| b.dir == dir) {
            if let Some(next) = self.append(dir, &base)? {
                self.durable = Some(Arc::new(next));
                return Ok(());
            }
        }
        self.checkpoint(dir)
    }
    /// Appends the delta since `base` as one journal record. `Ok(None)` means a
    /// full checkpoint is needed instead (no change, non-consecutive generation,
    /// journal too large, or the checkpoint changed on disk).
    fn append(&self, dir: &Path, base: &Durable) -> Result<Option<Durable>> {
        // Replay requires consecutive generations.
        if base.tail.records >= MAX_JOURNAL_RECORDS
            || base.saved.generation.checked_add(1) != Some(self.generation)
        {
            return Ok(None);
        }
        let delta = journal::Delta::between(&base.saved, self);
        if delta.is_noop(&base.saved) {
            // A save without changes exists to advance the previous
            // generation (spool cleanup does this before deleting files the
            // previous one might reference): only a checkpoint does that.
            return Ok(None);
        }
        let payload = serde_json::to_vec(&delta)?;
        let grown = base.tail.len.max(journal::HEADER_LEN)
            + payload.len() as u64
            + journal::RECORD_OVERHEAD;
        if grown > journal_limit(base.checkpoint_len)
            || checkpoint_identity(dir)?.as_deref() != Some(base.checkpoint.as_str())
        {
            return Ok(None);
        }
        let Some(tail) = journal::append(dir, &base.checkpoint, &base.tail, &payload)? else {
            return Ok(None);
        };
        super::crash::point("namespace.after_journal_append")?;
        Ok(Some(Durable {
            dir: dir.to_path_buf(),
            checkpoint: base.checkpoint.clone(),
            checkpoint_len: base.checkpoint_len,
            tail,
            saved: self.snapshot(),
        }))
    }
    /// Compaction: the verified current checkpoint and its journal become
    /// the previous generation, then the full namespace replaces the
    /// checkpoint and the (now stale) journal is removed.
    fn checkpoint(&mut self, dir: &Path) -> Result<()> {
        let path = dir.join("namespace.json");
        let journal = dir.join(journal::JOURNAL);
        if path.exists() {
            let bytes = fs::read(&path)?;
            if open_envelope(&bytes, false).is_err() {
                bail!("refusing to overwrite corrupt checkpoint");
            }
            // `load` parses and verifies it exactly like the primary.
            durable_bytes(&dir.join("namespace.previous.json"), &bytes)?;
            let previous_journal = dir.join(journal::PREVIOUS_JOURNAL);
            match fs::read(&journal) {
                Ok(records) => durable_bytes(&previous_journal, &records)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Bound to an older checkpoint, so already stale; tidy.
                    remove_if_present(&previous_journal)?;
                }
                Err(e) => return Err(e.into()),
            }
            super::crash::point("namespace.after_previous")?;
        }
        // One serialization serves both the hash and the file.
        let payload = serde_json::to_vec(self)?;
        let hash = blake3::hash(&payload).to_hex().to_string();
        let mut envelope = Vec::with_capacity(payload.len() + 128);
        envelope.extend_from_slice(ENVELOPE_V2);
        envelope.extend_from_slice(hash.as_bytes());
        envelope.extend_from_slice(PAYLOAD_KEY);
        envelope.extend_from_slice(&payload);
        envelope.push(b'}');
        durable_bytes(&path, &envelope)?;
        // The checkpoint now holds every journaled change: the journal names
        // the replaced checkpoint and `load` ignores it even if this fails.
        remove_if_present(&journal)?;
        self.durable = Some(Arc::new(Durable {
            dir: dir.to_path_buf(),
            checkpoint: hash,
            checkpoint_len: envelope.len() as u64,
            tail: journal::Tail::empty(),
            saved: self.snapshot(),
        }));
        Ok(())
    }
    /// [`Self::resolved`] without a copy: computed once per event map and
    /// shared by validation, `base`, `commit` and the drive's visible cache.
    pub(crate) fn projected(&self) -> Result<Projected> {
        self.resolved_shared()
    }
    /// Whether [`Self::projected`] is already computed for these events.
    pub(crate) fn has_projected(&self) -> bool {
        let cache = self.projection.0.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .as_ref()
            .is_some_and(|(key, version, _)| *version == self.version && self.events.is(key))
    }
    /// The cached projection for the current events and version, computed via
    /// [`resolve_events`] on a miss.
    fn resolved_shared(&self) -> Result<Projected> {
        let mut cache = self.projection.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((key, version, projected)) = &*cache {
            if *version == self.version && self.events.is(key) {
                return Ok(projected.clone());
            }
        }
        let projected = Arc::new(resolve_events(self.version, &self.events)?);
        *cache = Some((self.events.downgrade(), self.version, projected.clone()));
        Ok(projected)
    }
    /// Owned copy of the visible files (path -> resolved revision).
    pub(crate) fn resolved(&self) -> Result<BTreeMap<String, Resolved>> {
        Ok((*self.resolved_shared()?).clone())
    }
    /// Total size in bytes of the committed visible files (excludes pending
    /// intents). Reported as committed usage by `virtual_drive::capacity`.
    pub(crate) fn logical_used(&self) -> Result<u64> {
        self.resolved_shared()?
            .values()
            .filter_map(|r| r.event.content.as_ref())
            .try_fold(0u64, |sum, c| {
                sum.checked_add(c.size).context("logical usage overflow")
            })
    }
    /// Visible logical usage in bytes: committed files with pending writes
    /// and deletions applied. Used by `virtual_drive::capacity`.
    pub(crate) fn visible_logical_used(&self) -> Result<u64> {
        let resolved = self.resolved_shared()?;
        let mut sizes: BTreeMap<&str, u64> = resolved
            .iter()
            .filter_map(|(path, r)| r.event.content.as_ref().map(|c| (path.as_str(), c.size)))
            .collect();
        for intent in &self.pending {
            if intent.spool.is_some() {
                sizes.insert(&intent.path, intent.size);
            } else {
                sizes.remove(intent.path.as_str());
            }
        }
        sizes.values().try_fold(0u64, |sum, size| {
            sum.checked_add(*size).context("logical usage overflow")
        })
    }
    /// Publish parents before descendants so interrupted publication remains readable.
    pub(crate) fn unpublished_ordered(&self) -> Result<Vec<(String, Event)>> {
        let mut known = (*self.published).clone();
        let mut remaining: BTreeMap<_, _> = self
            .events
            .iter()
            .filter(|(id, _)| !known.contains(*id))
            .map(|(id, event)| (id.clone(), event.clone()))
            .collect();
        let mut ordered = vec![];
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .filter(|(_, e)| e.parents.iter().all(|p| known.contains(p)))
                .map(|(id, _)| id.clone())
                .collect();
            if ready.is_empty() {
                bail!("missing or cyclic publication ancestry");
            }
            for id in ready {
                let event = remaining.remove(&id).unwrap();
                known.insert(id.clone());
                ordered.push((id, event));
            }
        }
        Ok(ordered)
    }
    /// Adds validated events (from pool sync or history) to `events`; ids must
    /// match content, known ids are skipped, and the result must still reduce.
    pub(crate) fn ingest(&mut self, events: BTreeMap<String, Event>) -> Result<()> {
        let mut next = self.events.clone();
        for (id, event) in events {
            event.validate()?;
            if event.id()? != id {
                bail!("event identity mismatch");
            }
            // Ids are content hashes: a known id is this event. Skipping it
            // keeps an unchanged event map shared (and its projection cached).
            if next.contains_key(&id) {
                continue;
            }
            next.insert(id, event);
        }
        shared_model::reduce(&next)?;
        self.events = next;
        Ok(())
    }
    /// Ancestry for a new write to `path`: the recorded base if any, else the
    /// current visible revision, else the unreferenced heads at that path.
    /// Recorded in `bases`. Called by `VirtualDrive` writes.
    pub(crate) fn base(&mut self, path: &str) -> Result<Vec<String>> {
        valid_path(path)?;
        if let Some(base) = self.bases.get(path) {
            return Ok(base.clone());
        }
        let base = if let Some(r) = self.resolved_shared()?.get(path) {
            vec![r.event_id.clone()]
        } else {
            let referenced: BTreeSet<_> = self
                .events
                .values()
                .flat_map(|e| e.parents.iter())
                .collect();
            self.events
                .iter()
                .filter(|(id, e)| e.path == path && !referenced.contains(id))
                .map(|(id, _)| id.clone())
                .collect()
        };
        self.bases.insert(path.into(), base.clone());
        Ok(base)
    }
    /// Optimization baseline only: use the edit's captured ancestry, never the
    /// latest projected head (which may be a concurrent writer's revision).
    pub(crate) fn upload_base(&self, intent: &Intent) -> Result<Option<Content>> {
        let parents = match &intent.depends_on {
            Some(id) => vec![self
                .committed_intents
                .get(id)
                .context("previous local write not committed")?
                .clone()],
            None => intent.parents.clone(),
        };
        if parents.len() != 1 {
            return Ok(None);
        }
        // Imported/other-workspace archives may still be owned by a legacy GC.
        // Only this workspace's durable commit receipts prove the base was
        // produced under our no-GC peer policy. A remote base gets a full upload
        // on its first local edit; subsequent local edits may reuse it.
        let candidate = self
            .events
            .get(&parents[0])
            .filter(|event| event.path == intent.event_path)
            .and_then(|event| event.content.as_ref());
        let Some(content) = candidate else {
            return Ok(None);
        };
        // An incremental recipe rewrites the whole base archive: a pack
        // member is not its archive.
        if content.pack.is_some() {
            return Ok(None);
        }
        let fingerprint = crate::manifest::manifest_fingerprint(&content.manifest)?;
        // MOVE preserves content but creates a new namespace event. Follow the
        // original upload receipt, not the move receipt, and require the exact
        // recorded manifest so an archive name alone never grants provenance.
        for (receipt, event_id) in &self.committed_intents {
            if content.manifest.archive_id != format!("virtual-{receipt}") {
                continue;
            }
            if let Some(uploaded) = self.events.get(event_id).and_then(|e| e.content.as_ref()) {
                if crate::manifest::manifest_fingerprint(&uploaded.manifest)? == fingerprint {
                    return Ok(Some(content.clone()));
                }
            }
        }
        Ok(None)
    }
    /// Commits the leading pending deletions of `path` now: they upload
    /// nothing, so they need not wait for sync. Stops at the first pending
    /// write (it needs its upload) or unmet dependency.
    pub(crate) fn settle_deletions(&mut self, path: &str) -> Result<()> {
        while let Some(next) = self.pending.iter().find(|i| i.path == path).cloned() {
            let waiting = next
                .depends_on
                .as_ref()
                .is_some_and(|dep| !self.committed_intents.contains_key(dep));
            if next.spool.is_some() || waiting {
                break;
            }
            self.commit(&next, None)?;
        }
        Ok(())
    }
    /// Commits `intent` as a new event (with `content` for a write, `None` for a
    /// deletion), removes it from `pending`, records the receipt and returns the
    /// event id. Called by sync after upload and by `settle_deletions`.
    pub(crate) fn commit(&mut self, intent: &Intent, content: Option<Content>) -> Result<String> {
        let event = Event {
            version: content.as_ref().map_or(1, Content::event_version),
            worker: self.worker.clone(),
            device: self.device.clone(),
            path: intent.event_path.clone(),
            parents: match &intent.depends_on {
                Some(id) => vec![self
                    .committed_intents
                    .get(id)
                    .context("previous local write not committed")?
                    .clone()],
                None => intent.parents.clone(),
            },
            content,
        };
        event.validate()?;
        let id = event.id()?;
        self.events.insert(id.clone(), event);
        self.pending.retain(|p| p.id != intent.id);
        self.committed_intents.insert(intent.id.clone(), id.clone());
        // Generic DAV clients do not report native editor lifetimes. Do not
        // advance a conservative observed baseline merely because another PUT committed.
        // The projection is cached for the save that follows.
        self.resolved_shared()?;
        Ok(id)
    }
}

#[cfg(test)]
mod bench_tests;
#[cfg(test)]
mod journal_tests;
