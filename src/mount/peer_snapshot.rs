//! V7 runtime. Namespace is a local materialization, never a GC authority.
use super::namespace::{durable_json, random_id, valid_path, Intent};
use super::peer_snapshot_model::{self as model, Policy, Revision, Snapshot};
use super::peer_snapshot_transport::{GcJournal, Store};
use super::shared_model::{Content, Event};
use super::virtual_drive::VirtualDrive;
use crate::prelude::*;

#[path = "peer_snapshot_native.rs"]
mod native;
#[cfg(test)]
#[path = "peer_snapshot_tests.rs"]
mod tests;

trait Io {
    fn collect(&self, kind: &str, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>>;
    fn publish(&self, kind: &str, id: &str, bytes: &[u8]) -> Result<()>;
    fn copy(&self, owner: &str, revision: &str, manifest: &Manifest) -> Result<Manifest>;
    fn upload(&self, source: &Path, archive: &str) -> Result<Manifest>;
    fn verify(&self, manifest: &Manifest) -> Result<()>;
    fn capture(&self, manifest: &Manifest, output: &Path) -> Result<()>;
    fn gc(&self, path: &Path, proof: &mut dyn FnMut(&str, &str) -> Result<()>) -> Result<()>;
}
struct LiveIo<'a> {
    drive: &'a VirtualDrive,
    store: Store,
}
impl Io for LiveIo<'_> {
    fn collect(&self, k: &str, known: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        self.store.collect(k, known)
    }
    fn publish(&self, k: &str, id: &str, b: &[u8]) -> Result<()> {
        self.store.publish(k, id, b)
    }
    fn copy(&self, o: &str, r: &str, m: &Manifest) -> Result<Manifest> {
        self.store.copy_revision(o, r, m)
    }
    fn upload(&self, p: &Path, id: &str) -> Result<Manifest> {
        super::workspace::upload_eligible_tracked(
            &self.drive.rclone,
            &self.drive.policy,
            &self.drive.pool,
            p,
            id,
        )
        .map(|v| v.0)
    }
    fn verify(&self, m: &Manifest) -> Result<()> {
        let reader = crate::storage::reader::StorageReader::rclone(&self.drive.rclone);
        for s in &m.shards {
            // Every payload was read back when it was uploaded or copied.
            reader.verify_unchanged(s)?;
        }
        Ok(())
    }
    fn capture(&self, manifest: &Manifest, output: &Path) -> Result<()> {
        let metadata = output.with_extension("manifest.json");
        durable_json(&metadata, manifest)?;
        crate::commands::get(
            &self.drive.rclone,
            &metadata.to_string_lossy(),
            output,
            self.drive.policy.workers,
            self.drive.policy.retries,
        )
    }
    fn gc(&self, p: &Path, proof: &mut dyn FnMut(&str, &str) -> Result<()>) -> Result<()> {
        self.store.resume_gc(p, proof)
    }
}

fn hash<T: Serialize>(v: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(v)?).to_hex().to_string())
}
#[derive(Clone, Serialize, Deserialize)]
struct Checked<T> {
    hash: String,
    value: T,
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    durable_json(
        path,
        &Checked {
            hash: hash(value)?,
            value,
        },
    )
}
fn load<T: Serialize + serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let checked: Checked<T> = crate::utils::read_json(path)?;
    if checked.hash != hash(&checked.value)? {
        bail!("v7 local state checksum mismatch; retain workspace");
    }
    Ok(checked.value)
}
#[derive(Clone, Serialize, Deserialize)]
struct NameOp {
    nonce: String,
    // One immutable envelope changes every listed directory entry together.
    entries: BTreeMap<String, String>,
    parents: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Target {
    file: String,
    revision: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    snapshots: BTreeMap<String, Snapshot>,
    names: BTreeMap<String, NameOp>,
    mappings: BTreeMap<String, Target>,
    published_snapshots: BTreeSet<String>,
    published_names: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    file: String,
    owner: String,
    parents: BTreeSet<String>,
    revisions: BTreeMap<String, Revision>,
    name: Option<NameOp>,
    source_hash: Option<String>,
    payloads: BTreeMap<String, Manifest>,
    ready: Option<Snapshot>,
}
fn name_heads(state: &State, file: &str) -> BTreeSet<String> {
    let covered: BTreeSet<_> = state
        .names
        .values()
        .filter_map(|op| op.parents.get(file))
        .flatten()
        .cloned()
        .collect();
    state
        .names
        .iter()
        .filter(|(id, op)| op.entries.contains_key(file) && !covered.contains(*id))
        .map(|(id, _)| id.clone())
        .collect()
}
fn validate_names(state: &State) -> Result<()> {
    let mut done = BTreeSet::new();
    for (id, op) in &state.names {
        if hash(op)? != *id
            || op.entries.is_empty()
            || op.entries.keys().any(|f| !op.parents.contains_key(f))
        {
            bail!("invalid v7 namespace operation");
        }
        for (file, path) in &op.entries {
            valid_path(path)?;
            if !state.snapshots.values().any(|s| &s.file_id == file) {
                bail!("namespace snapshot not yet available; retry sync");
            }
            for parent in &op.parents[file] {
                if state
                    .names
                    .get(parent)
                    .is_none_or(|p| !p.entries.contains_key(file))
                {
                    bail!("namespace ancestry not yet available; retry sync");
                }
            }
        }
    }
    while done.len() < state.names.len() {
        let ready: Vec<_> = state
            .names
            .iter()
            .filter(|(id, op)| {
                !done.contains(*id) && op.parents.values().flatten().all(|p| done.contains(p))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if ready.is_empty() {
            bail!("cyclic namespace operations");
        }
        done.extend(ready);
    }
    Ok(())
}
fn frontier(state: &State, analysis: &model::Analysis, file: &str) -> BTreeSet<String> {
    analysis
        .frontier
        .iter()
        .filter(|id| state.snapshots[*id].file_id == file)
        .cloned()
        .collect()
}
impl VirtualDrive {
    fn snapshot_policy(&self) -> Result<Policy> {
        Ok(Policy {
            genesis_id: hash(&("rpool-private-v7", &self.pool_sync_roots))?,
            history_limit: self.pool_history_limit,
        })
    }
    fn snapshot_state(&self) -> Result<State> {
        let path = self.root.join("peer-v7-state.json");
        if path.exists() {
            load(&path)
        } else {
            Ok(State::default())
        }
    }
    fn save_snapshot_state(&self, state: &State) -> Result<()> {
        save(&self.root.join("peer-v7-state.json"), state)
    }
    fn snapshot_store(&self) -> Result<LiveIo<'_>> {
        Ok(LiveIo {
            drive: self,
            store: Store::new(&self.rclone, &self.pool_sync_roots)?
                .with_native_crypt(self.policy.native_crypt),
        })
    }
    pub(crate) fn begin_snapshot(
        &self,
        path: &str,
        visible: Option<&super::virtual_drive::Revision>,
    ) -> Result<Intent> {
        let store = self.snapshot_store()?;
        self.begin_snapshot_with(path, visible, &store)
    }
    fn begin_snapshot_with(
        &self,
        path: &str,
        visible: Option<&super::virtual_drive::Revision>,
        store: &dyn Io,
    ) -> Result<Intent> {
        let intent = self.begin_intent(path, visible)?;
        self.capture_originals(&intent, store)?;
        Ok(intent)
    }
    /// Captures the original payload of each parent revision of `intent` into
    /// its spool, so the edit's original survives history collection. An edit
    /// that continues this workspace's own intent (`depends_on`) needs none.
    fn capture_originals(&self, intent: &Intent, store: &dyn Io) -> Result<()> {
        if intent.depends_on.is_some() {
            return Ok(());
        }
        let state = self.snapshot_state()?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        for event in &intent.parents {
            let target = state
                .mappings
                .get(event)
                .context("stale write baseline; recover as a new file")?;
            let merged = analysis
                .merged
                .get(&target.file)
                .context("write baseline file missing")?;
            let revision = merged
                .revisions
                .get(&target.revision)
                .context("write baseline revision missing")?;
            if let Some(expected) = &revision.content_hash {
                let payload = merged
                    .payloads
                    .get(&target.revision)
                    .context("original already retired; recover edits as a new file")?;
                let dir = self
                    .root
                    .join("spool")
                    .join(&intent.id)
                    .join("captured")
                    .join(&target.revision);
                fs::create_dir_all(&dir)?;
                let output = dir.join("content");
                store.capture(&payload.manifest, &output)?;
                if fs::metadata(&output)?.len() != revision.size
                    || crate::utils::hash_file_range(&output, 0, revision.size)? != *expected
                {
                    bail!("captured original integrity failure");
                }
                crate::utils::sync_file(&output)?;
                // Persist the newly created anchor path before admitting the edit.
                // A durable file without durable directory entries can disappear
                // on power loss while its replacement remains in the local spool.
                #[cfg(unix)]
                for parent in dir.ancestors().take(4) {
                    File::open(parent)?.sync_all()?;
                }
            }
        }
        Ok(())
    }
    fn validate_snapshot_locations(&self, state: &State) -> Result<()> {
        let allowed = crate::remote_root::apply_remote_roots(self.policy.remotes.clone())?;
        for snapshot in state.snapshots.values() {
            for manifest in snapshot.payloads.values() {
                for shard in &manifest.shards {
                    let prefix = crate::utils::remote_join(
                        &shard.remote,
                        &format!("{}/", model::owner_archive(&snapshot.owner_id)),
                    );
                    let suffix = shard
                        .object
                        .strip_prefix(&prefix)
                        .context("snapshot object outside owner")?;
                    if !allowed.contains(&shard.remote)
                        || suffix
                            .split('/')
                            .any(|p| p.is_empty() || p == "." || p == ".." || p.contains('\\'))
                    {
                        bail!("snapshot destination outside configured pool/owner");
                    }
                }
            }
        }
        Ok(())
    }
    /// Resolve retained bytes without updating source metadata or contacting peers.
    pub(super) fn transition_snapshot_events(
        &self,
        intents: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        if !self.peer_retention {
            bail!("semantic transition validation requires v7");
        }
        let state = self.snapshot_state()?;
        self.validate_snapshot_locations(&state)?;
        validate_names(&state)?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let namespace = self.state.lock().unwrap();
        let mut allowed = BTreeSet::new();
        for intent in intents {
            let Some(committed) = namespace.committed_intents.get(intent) else {
                // Publication precedes the namespace commit. A crash can leave the
                // journal-owned sealed write and its ready snapshot already visible.
                // Admit only this fresh-file transition's exact checked plan, never
                // another equal-content file or an arbitrary remote snapshot.
                let Some(pending) = namespace.pending.iter().find(|p| &p.id == intent) else {
                    continue;
                };
                let path = self
                    .root
                    .join("spool")
                    .join(intent)
                    .join("snapshot-plan.json");
                if !path.exists() {
                    continue;
                }
                let plan: Plan = load(&path)?;
                let Some(ready) = &plan.ready else {
                    continue;
                };
                let (revision_id, semantic) = plan
                    .revisions
                    .iter()
                    .next()
                    .context("transition pending revision missing")?;
                if plan.file != pending.id
                    || !pending.parents.is_empty()
                    || pending.depends_on.is_some()
                    || pending.spool.is_none()
                    || !plan.parents.is_empty()
                    || plan.revisions.len() != 1
                    || plan.source_hash.as_deref() != Some(pending.hash.as_str())
                    || semantic.content_hash.as_deref() != Some(pending.hash.as_str())
                    || semantic.size != pending.size
                    || !semantic.parents.is_empty()
                    || semantic.worker != namespace.worker
                    || semantic.device != namespace.device
                    || semantic.id()? != *revision_id
                    || ready.file_id != plan.file
                    || ready.owner_id != plan.owner
                    || ready.revisions != plan.revisions
                    || !ready.parents.is_empty()
                {
                    bail!("transition pending snapshot plan identity mismatch");
                }
                let name = plan
                    .name
                    .as_ref()
                    .context("transition pending name missing")?;
                if name.entries != BTreeMap::from([(plan.file.clone(), pending.path.clone())])
                    || name.parents != BTreeMap::from([(plan.file.clone(), BTreeSet::new())])
                {
                    bail!("transition pending snapshot name mismatch");
                }
                // Absent publication is simply not yet a materialized event. Remote
                // collection may prove publication before local receipt persistence;
                // normal sync still must finish receipts and commit before activation.
                if !state.snapshots.contains_key(&ready.id()?)
                    || !state.names.contains_key(&hash(name)?)
                {
                    continue;
                }
                for (event_id, mapped) in &state.mappings {
                    if mapped.file != plan.file || mapped.revision != *revision_id {
                        continue;
                    }
                    if namespace.events.get(event_id).is_some_and(|event| {
                        event.path == pending.path
                            && event.content.as_ref().is_some_and(|content| {
                                content.hash == pending.hash && content.size == pending.size
                            })
                    }) {
                        allowed.insert(event_id.clone());
                    }
                }
                continue;
            };
            if !namespace.published.contains(committed) {
                bail!("transition snapshot commit is not published");
            }
            let target = state
                .mappings
                .get(committed)
                .context("transition commit mapping missing")?;
            let original = namespace
                .events
                .get(committed)
                .context("transition commit event missing")?;
            let semantic = analysis
                .merged
                .get(&target.file)
                .and_then(|m| m.revisions.get(&target.revision))
                .context("transition semantic revision missing")?;
            let snapshots: Vec<_> = state
                .snapshots
                .iter()
                .filter(|(_, snapshot)| snapshot.file_id == target.file)
                .collect();
            let names = name_heads(&state, &target.file);
            if snapshots.is_empty()
                || names.is_empty()
                || snapshots
                    .iter()
                    .any(|(id, _)| !state.published_snapshots.contains(*id))
                || names.iter().any(|id| !state.published_names.contains(id))
            {
                bail!("transition snapshot or name is not fully published");
            }
            for (event_id, mapped) in &state.mappings {
                if mapped.file != target.file || mapped.revision != target.revision {
                    continue;
                }
                let Some(event) = namespace.events.get(event_id) else {
                    continue;
                };
                let semantic_matches = match (&event.content, &semantic.content_hash) {
                    (Some(content), Some(hash)) => {
                        content.hash == *hash && content.size == semantic.size
                    }
                    (None, None) => true,
                    _ => false,
                };
                if event.path == original.path && semantic_matches {
                    allowed.insert(event_id.clone());
                }
            }
            if !allowed.contains(committed) {
                bail!("transition committed event differs from semantic revision");
            }
        }
        Ok(allowed)
    }

    pub(crate) fn recovery_snapshot_manifest(&self, event: &str) -> Result<Manifest> {
        let state = self.snapshot_state()?;
        self.validate_snapshot_locations(&state)?;
        let target = state
            .mappings
            .get(event)
            .context("source snapshot mapping missing")?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let merged = analysis
            .merged
            .get(&target.file)
            .context("source snapshot file missing")?;
        let semantic = merged
            .revisions
            .get(&target.revision)
            .context("source revision missing")?;
        let namespace = self.state.lock().unwrap();
        let content = namespace
            .events
            .get(event)
            .and_then(|e| e.content.as_ref())
            .context("source event content missing")?;
        if semantic.content_hash.as_deref() != Some(content.hash.as_str())
            || semantic.size != content.size
        {
            bail!("source materialized event differs from retained snapshot revision");
        }
        Ok(merged
            .payloads
            .get(&target.revision)
            .context("source revision is no longer retained")?
            .manifest
            .clone())
    }
    pub(crate) fn read_snapshot(&self, event: &str, offset: u64, count: usize) -> Result<Vec<u8>> {
        let state = self.snapshot_state()?;
        let target = state
            .mappings
            .get(event)
            .context("expired file handle; reopen file")?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let payload = analysis
            .merged
            .get(&target.file)
            .and_then(|m| m.payloads.get(&target.revision))
            .context(
                "historical version retired; reopen file (new content is never substituted)",
            )?;
        self.cache.read(
            &crate::storage::reader::StorageReader::rclone(&self.rclone),
            &payload.manifest,
            offset,
            count,
            self.policy.workers,
            self.policy.retries,
        )
    }
    pub(crate) fn pin_snapshot_read(&self, path: &str, event: &str) -> Result<()> {
        let state = self.snapshot_state()?;
        let target = state.mappings.get(event).context("expired file handle")?;
        let mut namespace = self.state.lock().unwrap();
        let current = namespace
            .snapshot_view
            .get(path)
            .and_then(|id| state.mappings.get(id))
            .context("file moved/deleted; reopen")?;
        if current.file != target.file || current.revision != target.revision {
            bail!("file changed; reopen");
        }
        let identity = hash(&(&target.file, &target.revision))?;
        let mut pins = self.peer_read_pins.lock().unwrap();
        if pins.get(path).is_some_and(|id| id != &identity) {
            bail!("file changed after read; remount before reading new content");
        }
        pins.insert(path.into(), identity);
        namespace.bases.insert(path.into(), vec![event.into()]);
        namespace.save(&self.root)?;
        Ok(())
    }
    fn refresh_snapshots(&self, state: &mut State, store: &dyn Io) -> Result<model::Analysis> {
        for (id, bytes) in store.collect("snapshots", &state.snapshots.keys().cloned().collect())? {
            let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
            if snapshot.id()? != id {
                bail!("v7 snapshot identity mismatch");
            }
            state.snapshots.insert(id, snapshot);
        }
        for (id, bytes) in store.collect("names", &state.names.keys().cloned().collect())? {
            let op: NameOp = serde_json::from_slice(&bytes)?;
            if hash(&op)? != id {
                bail!("v7 name identity mismatch");
            }
            state.names.insert(id, op);
        }
        self.validate_snapshot_locations(state)?;
        let result = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        validate_names(state)?;
        self.save_snapshot_state(state)?;
        Ok(result)
    }
    fn replicate_snapshots(&self, state: &mut State, store: &dyn Io) -> Result<()> {
        for (id, snapshot) in &state.snapshots {
            if !state.published_snapshots.contains(id) {
                store.publish("snapshots", id, &serde_json::to_vec(snapshot)?)?;
                state.published_snapshots.insert(id.clone());
            }
        }
        for (id, op) in &state.names {
            if !state.published_names.contains(id) {
                store.publish("names", id, &serde_json::to_vec(op)?)?;
                state.published_names.insert(id.clone());
            }
        }
        self.save_snapshot_state(state)
    }
    fn materialize_snapshots(&self, state: &mut State, analysis: &model::Analysis) -> Result<()> {
        let mut namespace = self.state.lock().unwrap();
        let mut next = namespace.clone();
        next.snapshot_view.clear();
        let mut occupied = BTreeSet::new();
        let mut tombstones = Vec::new();
        for (file, merged) in &analysis.merged {
            let names = name_heads(state, file);
            for name in names {
                let path = &state.names[&name].entries[file];
                let conflicting = merged.heads.len() > 1;
                let visible: BTreeSet<_> = merged
                    .heads
                    .iter()
                    .chain(merged.originals.iter())
                    .cloned()
                    .collect();
                for revision in visible {
                    let semantic = &merged.revisions[&revision];
                    if semantic.content_hash.is_none() {
                        if !conflicting {
                            let event = Event {
                                version: 1,
                                worker: semantic.worker.clone(),
                                device: hash(&(file, &revision))?,
                                path: path.clone(),
                                parents: vec![],
                                content: None,
                            };
                            let id = event.id()?;
                            next.events.insert(id.clone(), event);
                            state.mappings.insert(
                                id.clone(),
                                Target {
                                    file: file.clone(),
                                    revision: revision.clone(),
                                },
                            );
                            tombstones.push((path.clone(), id));
                        }
                        continue;
                    }
                    let Some(payload) = merged.payloads.get(&revision) else {
                        continue;
                    };
                    let original =
                        merged.originals.len() == 1 && merged.originals.contains(&revision);
                    let mut target = if !conflicting || original {
                        path.clone()
                    } else {
                        super::shared_model::peer_path(path, &semantic.worker, &revision, 0)?
                    };
                    let mut n = 0;
                    while occupied
                        .iter()
                        .any(|p: &String| super::shared_model::overlaps(p, &target))
                    {
                        n += 1;
                        target = super::shared_model::peer_path(
                            path,
                            &semantic.worker,
                            &hash(&(file, &revision, &name))?,
                            n,
                        )?;
                        if n > state.snapshots.len() + state.names.len() + 100 {
                            bail!("v7 path collision");
                        }
                    }
                    occupied.insert(target.clone());
                    let event = Event {
                        version: 1,
                        worker: semantic.worker.clone(),
                        device: hash(&(file, &revision))?,
                        path: target.clone(),
                        parents: vec![],
                        content: Some(Content {
                            hash: semantic.content_hash.clone().unwrap(),
                            size: semantic.size,
                            manifest: payload.manifest.clone(),
                        }),
                    };
                    let id = event.id()?;
                    next.events.insert(id.clone(), event);
                    next.snapshot_view.insert(target, id.clone());
                    state.mappings.insert(
                        id,
                        Target {
                            file: file.clone(),
                            revision: revision.clone(),
                        },
                    );
                }
            }
        }
        for (path, id) in tombstones {
            next.snapshot_view.entry(path).or_insert(id);
        }
        // Save mapping before the cache, so every acknowledged DAV baseline is interpretable.
        self.save_snapshot_state(state)?;
        next.save(&self.root)?;
        *namespace = next;
        Ok(())
    }
    pub(crate) fn pull_snapshots(&self) -> Result<()> {
        let store = self.snapshot_store()?;
        self.pull_snapshots_with(&store)
    }
    fn pull_snapshots_with(&self, store: &dyn Io) -> Result<()> {
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync gate poisoned"))?;
        let mut state = self.snapshot_state()?;
        let analysis = self.refresh_snapshots(&mut state, store)?;
        self.materialize_snapshots(&mut state, &analysis)
    }
    /// Read-only browse of a fresh workspace: the history limit is part of the
    /// snapshot policy identity and is chosen per mount, so adopt the one the
    /// pool's snapshots record before pulling. Remote access is `collect` only;
    /// state is saved solely in this (temporary) workspace. Returns whether the
    /// pool has any v7 records.
    pub(crate) fn pull_snapshots_adopting_policy(&mut self) -> Result<bool> {
        let limit = {
            let store = self.snapshot_store()?;
            self.seed_snapshots_with(&store)?
        };
        if !self.adopt_snapshot_policy(limit) {
            return Ok(false);
        }
        self.pull_snapshots()?;
        Ok(true)
    }
    fn adopt_snapshot_policy(&mut self, seeded: Option<Option<usize>>) -> bool {
        match seeded {
            None => false,
            Some(limit) => {
                if let Some(limit) = limit {
                    self.pool_history_limit = limit;
                }
                true
            }
        }
    }
    /// `None`: no v7 records at all. `Some(limit)`: the recorded snapshot limit
    /// (`None` when only name records exist).
    fn seed_snapshots_with(&self, store: &dyn Io) -> Result<Option<Option<usize>>> {
        let mut state = self.snapshot_state()?;
        for (id, bytes) in store.collect("snapshots", &state.snapshots.keys().cloned().collect())? {
            let snapshot: Snapshot = serde_json::from_slice(&bytes)?;
            if snapshot.id()? != id {
                bail!("v7 snapshot identity mismatch");
            }
            state.snapshots.insert(id, snapshot);
        }
        let genesis = self.snapshot_policy()?.genesis_id;
        let limits: BTreeSet<_> = state
            .snapshots
            .values()
            .filter(|s| s.policy.genesis_id == genesis)
            .map(|s| s.policy.history_limit)
            .collect();
        if limits.len() > 1 {
            bail!("pool snapshots record different history limits; cannot browse consistently");
        }
        if state.snapshots.is_empty() && store.collect("names", &BTreeSet::new())?.is_empty() {
            return Ok(None);
        }
        self.save_snapshot_state(&state)?;
        Ok(Some(limits.into_iter().next()))
    }
    fn make_plan(
        &self,
        state: &State,
        analysis: &model::Analysis,
        intent: &Intent,
    ) -> Result<Plan> {
        let namespace = self.state.lock().unwrap();
        let observed = match &intent.depends_on {
            Some(dep) => vec![namespace
                .committed_intents
                .get(dep)
                .context("pending predecessor not committed")?
                .clone()],
            None => intent.parents.clone(),
        };
        let mut targets = Vec::new();
        for id in observed {
            targets.push(
                state
                    .mappings
                    .get(&id)
                    .context("stale v7 edit baseline unavailable; recover local spool")?
                    .clone(),
            );
        }
        let file = targets
            .first()
            .map(|t| t.file.clone())
            .unwrap_or_else(|| intent.id.clone());
        if targets.iter().any(|t| t.file != file) {
            bail!("edit crosses independent file identities");
        }
        let parents = frontier(state, analysis, &file);
        let revision = Revision {
            parents: targets.iter().map(|t| t.revision.clone()).collect(),
            content_hash: intent.spool.as_ref().map(|_| intent.hash.clone()),
            size: if intent.spool.is_some() {
                intent.size
            } else {
                0
            },
            worker: namespace.worker.clone(),
            device: namespace.device.clone(),
        };
        let id = revision.id()?;
        let name = if name_heads(state, &file).is_empty() {
            Some(NameOp {
                nonce: random_id()?,
                entries: BTreeMap::from([(file.clone(), intent.path.clone())]),
                parents: BTreeMap::from([(file.clone(), BTreeSet::new())]),
            })
        } else {
            None
        };
        Ok(Plan {
            file,
            owner: random_id()?,
            parents,
            revisions: BTreeMap::from([(id, revision)]),
            name,
            source_hash: intent.spool.as_ref().map(|_| intent.hash.clone()),
            payloads: BTreeMap::new(),
            ready: None,
        })
    }
    fn finish_plan(
        &self,
        state: &State,
        store: &dyn Io,
        path: &Path,
        plan: &mut Plan,
        source: Option<&Path>,
    ) -> Result<Snapshot> {
        if let Some(snapshot) = &plan.ready {
            let mut candidate = state.clone();
            candidate.snapshots.insert(snapshot.id()?, snapshot.clone());
            self.validate_snapshot_locations(&candidate)?;
            model::analyze(&candidate.snapshots, &self.snapshot_policy()?)?;
            return Ok(snapshot.clone());
        }
        let policy = self.snapshot_policy()?;
        let needed = model::prepare(
            &plan.file,
            &policy,
            &plan.parents,
            &state.snapshots,
            &plan.revisions,
        )?;
        if let Some(source) = source {
            let expected = plan.source_hash.as_ref().context("missing sealed hash")?;
            let revision = plan
                .revisions
                .iter()
                .find(|(_, r)| r.content_hash.as_ref() == Some(expected))
                .context("source revision missing")?;
            if fs::metadata(source)?.len() != revision.1.size
                || crate::utils::hash_file_range(source, 0, revision.1.size)? != *expected
            {
                bail!("v7 sealed source changed");
            }
            if !plan.payloads.contains_key(revision.0) {
                let manifest = store.upload(source, &model::owner_archive(&plan.owner))?;
                plan.payloads.insert(revision.0.clone(), manifest);
                save(path, plan)?;
            }
        }
        let current = model::analyze(&state.snapshots, &policy)?;
        for revision in &needed.required {
            if plan.payloads.contains_key(revision) {
                continue;
            }
            let captured = path
                .parent()
                .context("plan directory missing")?
                .join("captured")
                .join(revision)
                .join("content");
            if captured.exists() {
                let semantic = needed
                    .revisions
                    .get(revision)
                    .context("captured revision missing")?;
                if fs::metadata(&captured)?.len() != semantic.size
                    || semantic.content_hash.as_ref()
                        != Some(&crate::utils::hash_file_range(&captured, 0, semantic.size)?)
                {
                    bail!("captured original changed");
                }
                let mut manifest = store.upload(
                    &captured,
                    &format!("{}/r-{revision}", model::owner_archive(&plan.owner)),
                )?;
                // Nested standalone manifests are retained metadata, never payload ownership.
                manifest.archive_id = model::owner_archive(&plan.owner);
                crate::manifest::validate_manifest(&manifest)?;
                plan.payloads.insert(revision.clone(), manifest);
                save(path, plan)?;
                continue;
            }
            if let Some(payload) = current
                .merged
                .get(&plan.file)
                .and_then(|m| m.payloads.get(revision))
                .or_else(|| needed.payloads.get(revision))
            {
                if state
                    .snapshots
                    .get(&payload.snapshot_id)
                    .is_none_or(|s| s.file_id != plan.file)
                {
                    bail!("copy source file identity mismatch");
                }
                let private = store.copy(&plan.owner, revision, &payload.manifest)?;
                plan.payloads.insert(revision.clone(), private);
                save(path, plan)?;
            } else if plan
                .revisions
                .values()
                .any(|r| r.parents.contains(revision))
            {
                bail!("historical original unavailable; preserve local edits and recover/re-save as a new file");
            }
        }
        let snapshot = model::build(
            plan.file.clone(),
            plan.owner.clone(),
            policy,
            plan.parents.clone(),
            &state.snapshots,
            plan.revisions.clone(),
            plan.payloads.clone(),
        )?;
        plan.ready = Some(snapshot.clone());
        save(path, plan)?;
        Ok(snapshot)
    }
    fn publish_plan(
        &self,
        state: &mut State,
        store: &dyn Io,
        plan: &Plan,
        snapshot: Snapshot,
    ) -> Result<()> {
        // A fully private closure is durable before the semantic record is visible.
        let id = snapshot.id()?;
        let mut candidate = state.clone();
        candidate.snapshots.insert(id.clone(), snapshot.clone());
        self.validate_snapshot_locations(&candidate)?;
        model::analyze(&candidate.snapshots, &self.snapshot_policy()?)?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        // Another peer may already have accepted and retired this exact snapshot.
        // Reconcile its semantic receipt without resurrecting deleted payloads.
        let retired = analysis.gc.contains_key(&id);
        if !retired {
            for manifest in snapshot.payloads.values() {
                store.verify(manifest)?;
            }
        }
        store.publish("snapshots", &id, &serde_json::to_vec(&snapshot)?)?;
        state.snapshots.insert(id.clone(), snapshot);
        state.published_snapshots.insert(id);
        if let Some(op) = &plan.name {
            let id = hash(op)?;
            store.publish("names", &id, &serde_json::to_vec(op)?)?;
            state.names.insert(id.clone(), op.clone());
            state.published_names.insert(id);
        }
        self.save_snapshot_state(state)
    }
    pub(crate) fn sync_snapshots(&self) -> Result<()> {
        let store = self.snapshot_store()?;
        self.sync_snapshots_with(&store)
    }
    fn sync_snapshots_with(&self, store: &dyn Io) -> Result<()> {
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync gate poisoned"))?;
        let mut state = self.snapshot_state()?;
        let mut analysis = self.refresh_snapshots(&mut state, store)?;
        self.replicate_snapshots(&mut state, store)?;
        let pending = self.state.lock().unwrap().pending.clone();
        for intent in pending {
            let dir = self.root.join("spool").join(&intent.id);
            fs::create_dir_all(&dir)?;
            let path = dir.join("snapshot-plan.json");
            let mut plan = if path.exists() {
                load(&path)?
            } else {
                let p = self.make_plan(&state, &analysis, &intent)?;
                save(&path, &p)?;
                p
            };
            if plan.source_hash != intent.spool.as_ref().map(|_| intent.hash.clone()) {
                bail!("v7 upload plan identity mismatch");
            }
            let source = self.spool_path(&intent);
            let snapshot = self.finish_plan(
                &state,
                store,
                &path,
                &mut plan,
                intent.spool.as_ref().map(|_| source.as_path()),
            )?;
            self.publish_plan(&mut state, store, &plan, snapshot.clone())?;
            let (revision, semantic) = plan
                .revisions
                .iter()
                .next()
                .context("intent revision missing")?;
            let content = semantic
                .content_hash
                .as_ref()
                .map(|h| {
                    snapshot.payloads.get(revision).map(|m| Content {
                        hash: h.clone(),
                        size: semantic.size,
                        manifest: m.clone(),
                    })
                })
                .flatten();
            let mut namespace = self.state.lock().unwrap();
            let mut next = namespace.clone();
            let event = next.commit(&intent, content.clone())?;
            state.mappings.insert(
                event.clone(),
                Target {
                    file: plan.file.clone(),
                    revision: revision.clone(),
                },
            );
            self.save_snapshot_state(&state)?;
            next.published.insert(event.clone());
            if intent.spool.is_none() {
                next.bases.insert(intent.path.clone(), vec![event.clone()]);
            }
            next.save(&self.root)?;
            *namespace = next;
            drop(namespace);
            if let Some(content) = content {
                let mut pins = self.pins.lock().unwrap();
                if pins.get(&intent.path).is_some_and(|r| r.id() == intent.id) {
                    pins.insert(
                        intent.path.clone(),
                        super::virtual_drive::Revision::Cloud { id: event, content },
                    );
                }
            }
            analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        }
        // Collapse incomparable heads and transient original anchors into private closures.
        // A stable single snapshot with exactly its retained closure is never copied again.
        let compact = self.root.join("snapshot-compaction");
        fs::create_dir_all(&compact)?;
        let files: Vec<_> = analysis.merged.keys().cloned().collect();
        for file in files {
            let parents = frontier(&state, &analysis, &file);
            let merged = &analysis.merged[&file];
            let history_count = merged.history.len();
            let actual: BTreeSet<_> = parents
                .iter()
                .flat_map(|id| state.snapshots[id].payloads.keys().cloned())
                .collect();
            let wanted: BTreeSet<_> = merged
                .required
                .difference(&merged.unavailable)
                .cloned()
                .collect();
            if parents.len() == 1 && actual == wanted {
                continue;
            }
            eprintln!("[v7 compact] preserving {history_count} historical revisions plus current/conflicts");
            let path = compact.join(format!("{}.json", hash(&parents)?));
            let mut plan: Plan = if path.exists() {
                load(&path)?
            } else {
                let p = Plan {
                    file: file.clone(),
                    owner: random_id()?,
                    parents,
                    revisions: BTreeMap::new(),
                    name: None,
                    source_hash: None,
                    payloads: BTreeMap::new(),
                    ready: None,
                };
                save(&path, &p)?;
                p
            };
            let snapshot = self.finish_plan(&state, store, &path, &mut plan, None)?;
            self.publish_plan(&mut state, store, &plan, snapshot)?;
            analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        }
        self.materialize_snapshots(&mut state, &analysis)?;
        let gc = self.root.join("snapshot-gc");
        fs::create_dir_all(&gc)?;
        for (old, successor) in &analysis.gc {
            let snapshot = &state.snapshots[old];
            let path = gc.join(format!("{old}.json"));
            if !path.exists() {
                let metadata: BTreeSet<_> = snapshot
                    .payloads
                    .values()
                    .flat_map(|m| {
                        m.shards.iter().map(|s| {
                            crate::utils::remote_join(
                                &s.remote,
                                &format!(
                                    "{}/manifest.json",
                                    model::owner_archive(&snapshot.owner_id)
                                ),
                            )
                        })
                    })
                    .collect();
                GcJournal::prepare_with_metadata(
                    &path,
                    old,
                    successor,
                    &snapshot.owner_id,
                    &snapshot.payloads.values().cloned().collect::<Vec<_>>(),
                    &metadata.into_iter().collect::<Vec<_>>(),
                )?;
            }
            store.gc(&path, &mut |predecessor, proof| {
                if predecessor != old {
                    bail!("GC journal predecessor mismatch");
                }
                let s = state.snapshots.get(proof).context("GC proof missing")?;
                if !s.covered.contains(predecessor) {
                    bail!("GC causal proof changed");
                }
                store.publish("snapshots", proof, &serde_json::to_vec(s)?)
            })?;
        }
        self.cleanup_committed_spool()?;
        Ok(())
    }
    pub(crate) fn rename_snapshot(&self, from: &str, to: &str, directory: bool) -> Result<()> {
        valid_path(from)?;
        valid_path(to)?;
        if from == to {
            return Ok(());
        }
        if directory && (to.starts_with(&format!("{from}/")) || from.starts_with(&format!("{to}/")))
        {
            bail!("overlapping move");
        }
        let _gate = self
            .sync_gate
            .lock()
            .map_err(|_| anyhow!("sync gate poisoned"))?;
        if !self.state.lock().unwrap().pending.is_empty() {
            bail!("sync pending edits before moving files; local data retained");
        }
        let mut state = self.snapshot_state()?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let namespace = self.state.lock().unwrap();
        if namespace.snapshot_view.keys().any(|p| {
            (p == to || (directory && p.starts_with(&format!("{to}/"))))
                && namespace.events[&namespace.snapshot_view[p]]
                    .content
                    .is_some()
        }) {
            bail!("move destination exists");
        }
        let mut entries = BTreeMap::new();
        let mut parents = BTreeMap::new();
        for (path, event) in &namespace.snapshot_view {
            if namespace.events[event].content.is_none() {
                continue;
            }
            if path == from || (directory && path.starts_with(&format!("{from}/"))) {
                let target = state.mappings.get(event).context("missing move identity")?;
                if analysis
                    .merged
                    .get(&target.file)
                    .is_none_or(|m| m.heads.len() != 1)
                    || name_heads(&state, &target.file).len() != 1
                {
                    bail!("resolve conflicting content/names before moving this file");
                }
                let destination = format!("{to}{}", &path[from.len()..]);
                if entries.insert(target.file.clone(), destination).is_some() {
                    bail!("resolve file aliases before directory move");
                }
                parents.insert(target.file.clone(), name_heads(&state, &target.file));
            }
        }
        drop(namespace);
        if entries.is_empty() {
            bail!("move source not found");
        }
        let op = NameOp {
            nonce: random_id()?,
            entries,
            parents,
        };
        state.names.insert(hash(&op)?, op);
        validate_names(&state)?;
        self.save_snapshot_state(&state)?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        self.materialize_snapshots(&mut state, &analysis)
    }
    pub(crate) fn import_snapshot(&self, source: &str) -> Result<()> {
        let manifest = crate::manifest::load_manifest(&self.rclone, source)?;
        valid_path(&manifest.original_name)?;
        let visible = self.view()?.get(&manifest.original_name).cloned();
        if visible.is_some() {
            bail!("import destination already exists");
        }
        let intent = self.begin_observed(&manifest.original_name, None)?;
        let output = self.spool_path(&intent);
        crate::commands::get(
            &self.rclone,
            source,
            &output,
            self.policy.workers,
            self.policy.retries,
        )?;
        self.seal(intent)
    }
    pub(crate) fn snapshot_conflicts(&self) -> Result<Vec<super::peer_projection::Conflict>> {
        let state = self.snapshot_state()?;
        let analysis = model::analyze(&state.snapshots, &self.snapshot_policy()?)?;
        let namespace = self.state.lock().unwrap();
        let mut out = vec![];
        for (file, m) in &analysis.merged {
            if m.heads.len() < 2 {
                continue;
            }
            let path = name_heads(&state, file)
                .first()
                .map(|id| state.names[id].entries[file].clone())
                .unwrap_or_else(|| file.clone());
            let display = |revision: &String| {
                namespace
                    .snapshot_view
                    .iter()
                    .find(|(_, event)| {
                        state
                            .mappings
                            .get(*event)
                            .is_some_and(|t| &t.file == file && &t.revision == revision)
                    })
                    .map(|(p, _)| p.clone())
            };
            out.push(super::peer_projection::Conflict {
                path,
                originals: m.originals.iter().filter_map(display).collect(),
                branches: m
                    .heads
                    .iter()
                    .map(|id| super::peer_projection::Branch {
                        revision: id.clone(),
                        worker: m.revisions[id].worker.clone(),
                        device: m.revisions[id].device.clone(),
                        file: display(id),
                    })
                    .collect(),
                original_unavailable: m.originals.is_empty()
                    || m.originals.iter().any(|id| m.unavailable.contains(id)),
                ambiguous_original: m.originals.len() > 1,
            });
        }
        Ok(out)
    }
}
