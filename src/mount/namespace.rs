//! Metadata-only namespace. Cache absence is never interpreted as deletion.
use super::shared_model::{self, Content, Event, Resolved};
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Intent {
    pub id: String,
    pub path: String,
    pub event_path: String,
    pub parents: Vec<String>,
    pub spool: Option<String>,
    pub size: u64,
    pub hash: String,
    pub depends_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_serial: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CheckpointCursor {
    pub owner: String,
    pub serial: u64,
    pub hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Namespace {
    pub version: u32,
    pub generation: u64,
    pub device: String,
    pub worker: String,
    pub events: BTreeMap<String, Event>,
    pub published: BTreeSet<String>,
    pub pending: Vec<Intent>,
    pub committed_intents: BTreeMap<String, String>,
    pub directories: BTreeSet<String>,
    /// Conservative edit ancestry: listing never advances a writer's baseline.
    pub bases: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointCursor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checkpoint_ids: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accepted_spool: BTreeMap<String, Intent>,
    /// V7 materialized namespace only; authoritative file identities live in snapshots.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub snapshot_view: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    hash: String,
    payload: Namespace,
}

pub(crate) fn random_id() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("random identity: {e}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
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
pub(crate) fn durable_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().context("state parent missing")?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut temp, value)?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("flush temporary for {}", path.display()))?;
    super::crash::point("durable.before_persist")?;
    crate::utils::persist_replacing(temp, path)?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}
impl Namespace {
    pub(crate) fn create(worker: &str) -> Result<Self> {
        valid_path(worker)?;
        Ok(Self {
            version: 3,
            generation: 0,
            device: random_id()?,
            worker: worker.into(),
            events: BTreeMap::new(),
            published: BTreeSet::new(),
            pending: vec![],
            committed_intents: BTreeMap::new(),
            directories: BTreeSet::new(),
            bases: BTreeMap::new(),
            checkpoint: None,
            checkpoint_ids: BTreeMap::new(),
            accepted_spool: BTreeMap::new(),
            snapshot_view: BTreeMap::new(),
        })
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !matches!(self.version, 3..=7) {
            bail!("unsupported virtual namespace version");
        }
        let valid_id = |id: &str| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit());
        if let Some(cursor) = &self.checkpoint {
            if self.version != 5 || !valid_id(&cursor.owner) || !valid_id(&cursor.hash) {
                bail!("invalid checkpoint cursor");
            }
        }
        if self
            .checkpoint_ids
            .iter()
            .any(|(k, v)| !valid_id(k) || !valid_id(v))
        {
            bail!("invalid checkpoint revision map");
        }
        for (id, intent) in &self.accepted_spool {
            if self.version != 5
                || id != &intent.id
                || !valid_id(id)
                || intent.spool.as_ref().is_some_and(|s| s != id)
                || (intent.spool.is_some() && !valid_id(&intent.hash))
            {
                bail!("invalid acknowledged spool identity");
            }
            valid_path(&intent.path)?;
            valid_path(&intent.event_path)?;
        }
        valid_path(&self.worker)?;
        if self.version == 7 {
            for (id, event) in &self.events {
                event.validate()?;
                if event.id()? != *id {
                    bail!("invalid snapshot cache identity");
                }
            }
            for (path, id) in &self.snapshot_view {
                valid_path(path)?;
                if self.events.get(id).is_none_or(|e| e.path != *path) {
                    bail!("invalid snapshot namespace view");
                }
            }
        } else {
            shared_model::reduce(&self.events)?;
        }
        if self
            .published
            .iter()
            .any(|id| !self.events.contains_key(id))
        {
            bail!("unknown published revision");
        }
        for (path, parents) in &self.bases {
            valid_path(path)?;
            if self.version != 5 && parents.iter().any(|id| !self.events.contains_key(id)) {
                bail!("missing observed ancestry");
            }
        }
        for (id, event) in &self.committed_intents {
            if id.len() != 64
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
                || !self.events.contains_key(event)
            {
                bail!("invalid committed intent reference");
            }
        }
        let mut ids = BTreeSet::new();
        for intent in &self.pending {
            valid_path(&intent.path)?;
            valid_path(&intent.event_path)?;
            if self.version == 5
                && (intent.checkpoint_serial.is_none()
                    || intent
                        .checkpoint_base
                        .as_ref()
                        .is_some_and(|id| !valid_id(id)))
            {
                bail!("invalid checkpoint intent");
            }
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
        let mut live: BTreeSet<String> = self
            .resolved()?
            .into_iter()
            .filter(|(_, r)| r.event.content.is_some())
            .map(|(p, _)| p)
            .collect();
        for intent in &self.pending {
            if intent.spool.is_some() {
                live.insert(intent.path.clone());
            } else {
                live.remove(&intent.path);
            }
        }
        let mut folded = BTreeSet::new();
        for path in &live {
            if !folded.insert(path.to_lowercase()) {
                bail!("case collision in pending namespace");
            }
        }
        let mut spellings = BTreeMap::new();
        for path in live.iter().chain(self.directories.iter()) {
            valid_path(path)?;
            let mut prefix = String::new();
            for part in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if spellings
                    .insert(prefix.to_lowercase(), prefix.clone())
                    .is_some_and(|old| old != prefix)
                {
                    bail!("directory case collision in pending namespace");
                }
            }
            let parts: Vec<_> = path.split('/').collect();
            for i in 1..parts.len() {
                if folded.contains(&parts[..i].join("/").to_lowercase()) {
                    bail!("file/directory collision in pending namespace");
                }
            }
        }
        for directory in &self.directories {
            if folded.contains(&directory.to_lowercase()) {
                bail!("directory/file collision");
            }
        }
        Ok(())
    }
    pub(crate) fn load(dir: &Path, worker: &str) -> Result<Self> {
        let primary = dir.join("namespace.json");
        if !primary.exists() && !dir.join("namespace.previous.json").exists() {
            let mut fresh = Self::create(worker)?;
            fresh.save(dir)?;
            return Ok(fresh);
        }
        for filename in ["namespace.json", "namespace.previous.json"] {
            let attempt: Result<Self> = (|| {
                let envelope: Envelope = crate::utils::read_json(&dir.join(filename))?;
                if blake3::hash(&serde_json::to_vec(&envelope.payload)?)
                    .to_hex()
                    .as_str()
                    != envelope.hash
                {
                    bail!("namespace checksum mismatch");
                }
                envelope.payload.validate()?;
                Ok(envelope.payload)
            })();
            if let Ok(mut state) = attempt {
                state.worker = worker.into();
                if filename != "namespace.json" {
                    // A previous generation can omit acknowledged writes. Do not
                    // auto-publish or delete anything while the primary is corrupt.
                    bail!("primary namespace corrupt; validated previous generation exists at {}. Preserve spool and restore explicitly",dir.join(filename).display());
                }
                return Ok(state);
            }
        }
        bail!("no valid namespace checkpoint; preserve spool and cache")
    }
    pub(crate) fn save(&mut self, dir: &Path) -> Result<()> {
        self.validate()?;
        self.generation = self
            .generation
            .checked_add(1)
            .context("namespace generation overflow")?;
        let path = dir.join("namespace.json");
        if path.exists() {
            let old: Envelope = crate::utils::read_json(&path)?;
            if blake3::hash(&serde_json::to_vec(&old.payload)?)
                .to_hex()
                .as_str()
                != old.hash
            {
                bail!("refusing to overwrite corrupt checkpoint");
            }
            durable_json(&dir.join("namespace.previous.json"), &old)?;
            super::crash::point("namespace.after_previous")?;
        }
        let envelope = Envelope {
            hash: blake3::hash(&serde_json::to_vec(self)?)
                .to_hex()
                .to_string(),
            payload: self.clone(),
        };
        durable_json(&path, &envelope)
    }
    pub(crate) fn resolved(&self) -> Result<BTreeMap<String, Resolved>> {
        if self.version == 7 {
            self.snapshot_view
                .iter()
                .map(|(path, id)| {
                    Ok((
                        path.clone(),
                        Resolved {
                            event_id: id.clone(),
                            event: self
                                .events
                                .get(id)
                                .context("snapshot view entry missing")?
                                .clone(),
                        },
                    ))
                })
                .collect()
        } else if self.version == 6 {
            Ok(super::peer_projection::project(&self.events)?.files)
        } else {
            shared_model::reduce(&self.events)
        }
    }
    pub(crate) fn logical_used(&self) -> Result<u64> {
        self.resolved()?
            .values()
            .filter_map(|r| r.event.content.as_ref())
            .try_fold(0u64, |sum, c| {
                sum.checked_add(c.size).context("logical usage overflow")
            })
    }
    pub(crate) fn visible_logical_used(&self) -> Result<u64> {
        let mut sizes: BTreeMap<_, _> = self
            .resolved()?
            .into_iter()
            .filter_map(|(path, r)| r.event.content.map(|c| (path, c.size)))
            .collect();
        for intent in &self.pending {
            if intent.spool.is_some() {
                sizes.insert(intent.path.clone(), intent.size);
            } else {
                sizes.remove(&intent.path);
            }
        }
        sizes.values().try_fold(0u64, |sum, size| {
            sum.checked_add(*size).context("logical usage overflow")
        })
    }
    /// Publish parents before descendants so interrupted publication remains readable.
    pub(crate) fn unpublished_ordered(&self) -> Result<Vec<(String, Event)>> {
        let mut known = self.published.clone();
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
    pub(crate) fn ingest(&mut self, events: BTreeMap<String, Event>) -> Result<()> {
        let mut next = self.events.clone();
        for (id, event) in events {
            event.validate()?;
            if event.id()? != id {
                bail!("event identity mismatch");
            }
            next.insert(id, event);
        }
        shared_model::reduce(&next)?;
        self.events = next;
        Ok(())
    }
    pub(crate) fn base(&mut self, path: &str) -> Result<Vec<String>> {
        valid_path(path)?;
        if let Some(base) = self.bases.get(path) {
            return Ok(base.clone());
        }
        let base = if let Some(r) = self.resolved()?.get(path) {
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
    pub(crate) fn commit(&mut self, intent: &Intent, content: Option<Content>) -> Result<String> {
        let event = Event {
            version: 1,
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
        self.resolved()?;
        Ok(id)
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn pre_checkpoint_namespace_checksum_remains_loadable_with_pending_intent() {
        let tmp = tempfile::tempdir().unwrap();
        let id = "a".repeat(64);
        // Literal pre-v5 field order and absent extension fields, not a new-schema serializer.
        let old = format!(
            r#"{{"version":3,"generation":1,"device":"{id}","worker":"old","events":{{}},"published":[],"pending":[{{"id":"{id}","path":"file","event_path":"file","parents":[],"spool":"{id}","size":0,"hash":"","depends_on":null}}],"committed_intents":{{}},"directories":[],"bases":{{}}}}"#
        );
        let hash = blake3::hash(old.as_bytes()).to_hex().to_string();
        fs::write(
            tmp.path().join("namespace.json"),
            format!(r#"{{"hash":"{hash}","payload":{old}}}"#),
        )
        .unwrap();
        let state = Namespace::load(tmp.path(), "old").unwrap();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(serde_json::to_string(&state).unwrap(), old);
        assert!(state.pending[0].checkpoint_serial.is_none());
    }
}
