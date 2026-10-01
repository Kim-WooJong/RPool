//! Pure authoritative-checkpoint model. Only the configured coordinator commits.
use super::shared_model::{self, Content, Event};
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ManagedContent {
    pub content: Content,
    /// Exact owned shard and metadata objects; empty denotes an external archive.
    pub objects: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Version {
    pub id: String,
    pub value: ManagedContent,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub version: u32,
    pub owner: String,
    pub serial: u64,
    pub files: BTreeMap<String, Version>,
    pub history: BTreeMap<String, Vec<Version>>,
    pub receipts: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Proposal {
    pub version: u32,
    pub id: String,
    pub serial: u64,
    pub device: String,
    pub worker: String,
    pub path: String,
    pub base: Option<String>,
    pub value: Option<ManagedContent>,
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_path(path: &str) -> Result<()> {
    Event {
        version: 1,
        worker: "checkpoint".into(),
        device: "checkpoint".into(),
        path: path.into(),
        parents: vec![],
        content: None,
    }
    .validate()
}
fn epoch_identity(id: &str) -> Result<(&str, u64, &str, &str)> {
    let parts: Vec<_> = id.split('-').collect();
    if parts.len() != 5
        || parts[0] != "epoch"
        || !valid_id(parts[1])
        || !valid_id(parts[3])
        || !valid_id(parts[4])
    {
        bail!("invalid checkpoint-owned archive identity");
    }
    let serial: u64 = parts[2].parse().context("invalid archive serial")?;
    if serial.to_string() != parts[2] {
        bail!("noncanonical archive serial");
    }
    Ok((parts[1], serial, parts[3], parts[4]))
}
impl ManagedContent {
    pub(crate) fn validate(&self) -> Result<()> {
        Event {
            version: 1,
            worker: "checkpoint".into(),
            device: "checkpoint".into(),
            path: "content".into(),
            parents: vec![],
            content: Some(self.content.clone()),
        }
        .validate()?;
        if self.objects.is_empty() {
            return Ok(());
        }
        let archive = &self.content.manifest.archive_id;
        epoch_identity(archive)?;
        let mut seen = BTreeSet::new();
        for object in &self.objects {
            let (remote, path) = object
                .split_once(':')
                .context("owned object must be remote:path")?;
            if remote.is_empty()
                || remote.starts_with('-')
                || !remote
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-. ".contains(&b))
                || path.contains(':')
                || path.contains('\\')
                || object.chars().any(char::is_control)
                || path.split('/').any(|p| matches!(p, "." | ".."))
                || !path.split('/').any(|p| p == archive)
                || !seen.insert(object.clone())
            {
                bail!("invalid owned object address");
            }
        }
        if self
            .content
            .manifest
            .shards
            .iter()
            .any(|s| !seen.contains(&s.object))
        {
            bail!("owned archive omits a shard object");
        }
        Ok(())
    }
}
impl Proposal {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 5
            || !valid_id(&self.id)
            || !valid_id(&self.device)
            || self.base.as_deref().is_some_and(|v| !valid_id(v))
        {
            bail!("invalid checkpoint proposal identity or version");
        }
        Event {
            version: 1,
            worker: self.worker.clone(),
            device: self.device.clone(),
            path: self.path.clone(),
            parents: vec![],
            content: None,
        }
        .validate()?;
        if let Some(value) = &self.value {
            value.validate()?;
            if !value.objects.is_empty() {
                let (_, serial, device, intent) =
                    epoch_identity(&value.content.manifest.archive_id)?;
                if serial != self.serial || device != self.device || intent != self.id {
                    bail!("proposal archive ownership mismatch");
                }
            }
        }
        Ok(())
    }
}
fn versions(checkpoint: &Checkpoint) -> impl Iterator<Item = &Version> {
    checkpoint
        .files
        .values()
        .chain(checkpoint.history.values().flatten())
}
fn overlaps(a: &str, b: &str) -> bool {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    a == b
        || a.strip_prefix(&b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|s| s.starts_with('/'))
}
impl Checkpoint {
    pub(crate) fn empty(owner: &str) -> Self {
        Self {
            version: 5,
            owner: owner.into(),
            serial: 0,
            files: BTreeMap::new(),
            history: BTreeMap::new(),
            receipts: BTreeSet::new(),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 5
            || !valid_id(&self.owner)
            || self.receipts.iter().any(|id| !valid_id(id))
        {
            bail!("invalid checkpoint identity or version");
        }
        for path in self.files.keys().chain(self.history.keys()) {
            validate_path(path)?;
        }
        for version in versions(self) {
            if !valid_id(&version.id) {
                bail!("invalid checkpoint revision identity");
            }
            version.value.validate()?;
        }
        let mut events = BTreeMap::new();
        for (path, version) in &self.files {
            let event = Event {
                version: 1,
                worker: "checkpoint".into(),
                device: self.owner.clone(),
                path: path.clone(),
                parents: vec![],
                content: Some(version.value.content.clone()),
            };
            events.insert(event.id()?, event);
        }
        shared_model::reduce(&events)?;
        Ok(())
    }
    /// Apply requests against this serial; stale requests are deliberately not acknowledged.
    pub(crate) fn apply_batch(
        &self,
        proposals: &[Proposal],
        keep_previous: usize,
    ) -> Result<(Self, Vec<String>)> {
        self.apply(proposals, keep_previous, !proposals.is_empty())
    }
    /// Coordinator maintenance explicitly advances the serial, even without requests.
    pub(crate) fn compact(&self, keep_previous: usize) -> Result<(Self, Vec<String>)> {
        self.apply(&[], keep_previous, true)
    }
    fn apply(
        &self,
        proposals: &[Proposal],
        keep: usize,
        advance: bool,
    ) -> Result<(Self, Vec<String>)> {
        self.validate()?;
        let mut ordered: Vec<_> = proposals.iter().collect();
        ordered.sort_by(|a, b| a.id.cmp(&b.id));
        let mut ids = BTreeSet::new();
        for proposal in &ordered {
            proposal.validate()?;
            if !ids.insert(&proposal.id) {
                bail!("duplicate proposal identity");
            }
            if let Some(value) = &proposal.value {
                if value.objects.is_empty() {
                    let fingerprint =
                        crate::manifest::manifest_fingerprint(&value.content.manifest)?;
                    let known = versions(self).any(|v| {
                        v.value.content.hash == value.content.hash
                            && v.value.content.size == value.content.size
                            && crate::manifest::manifest_fingerprint(&v.value.content.manifest)
                                .is_ok_and(|fp| fp == fingerprint)
                    });
                    if !known {
                        bail!("proposal introduces an unowned archive");
                    }
                }
            }
        }
        let mut next = self.clone();
        if advance {
            next.serial = next
                .serial
                .checked_add(1)
                .context("checkpoint serial overflow")?;
        }
        next.receipts.clear();
        for proposal in ordered {
            if proposal.serial != self.serial {
                continue;
            }
            let matches = next.files.get(&proposal.path).map(|v| &v.id) == proposal.base.as_ref();
            if matches {
                if let Some(old) = next.files.remove(&proposal.path) {
                    next.history
                        .entry(proposal.path.clone())
                        .or_default()
                        .insert(0, old);
                }
                if let Some(value) = &proposal.value {
                    next.files.insert(
                        proposal.path.clone(),
                        Version {
                            id: proposal.id.clone(),
                            value: value.clone(),
                        },
                    );
                }
            } else if let Some(value) = &proposal.value {
                let mut attempt = 0;
                let target = loop {
                    let target = shared_model::conflict_path(
                        &proposal.path,
                        &proposal.worker,
                        &proposal.id,
                        attempt,
                    )?;
                    if !next.files.keys().any(|p| overlaps(p, &target)) {
                        break target;
                    }
                    attempt += 1;
                    if attempt > next.files.len() + 1 {
                        bail!("cannot allocate checkpoint conflict path");
                    }
                };
                next.files.insert(
                    target,
                    Version {
                        id: proposal.id.clone(),
                        value: value.clone(),
                    },
                );
            }
            // A mismatching deletion is rejected without deleting current content.
            next.receipts.insert(proposal.id.clone());
        }
        for history in next.history.values_mut() {
            history.truncate(keep);
        }
        next.history
            .retain(|path, history| !history.is_empty() && next.files.contains_key(path));
        next.validate()?;
        let protected_ids: BTreeSet<_> = versions(&next)
            .map(|v| v.value.content.manifest.archive_id.clone())
            .collect();
        let protected: BTreeSet<_> = versions(&next)
            .flat_map(|v| {
                v.value.objects.iter().cloned().chain(
                    v.value
                        .content
                        .manifest
                        .shards
                        .iter()
                        .map(|s| s.object.clone()),
                )
            })
            .collect();
        let mut obsolete = BTreeSet::new();
        for value in versions(self)
            .map(|v| &v.value)
            .chain(proposals.iter().filter_map(|p| p.value.as_ref()))
        {
            if !protected_ids.contains(&value.content.manifest.archive_id) {
                obsolete.extend(
                    value
                        .objects
                        .iter()
                        .filter(|o| !protected.contains(*o))
                        .cloned(),
                );
            }
        }
        Ok((next, obsolete.into_iter().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u64) -> String {
        format!("{n:064x}")
    }
    fn proposal(serial: u64, n: u64, base: Option<String>) -> Proposal {
        let archive = format!("epoch-{}-{serial}-{}-{}", id(1000), id(1001), id(n));
        let manifest = Manifest {
            version: 2,
            archive_id: archive.clone(),
            original_name: "file".into(),
            original_size: 0,
            shard_size: 1024,
            created_unix: n,
            content_root_blake3: crate::manifest::content_root_v2(0, 1024, &None, &[]),
            coding: None,
            shards: vec![],
        };
        Proposal {
            version: 5,
            id: id(n),
            serial,
            device: id(1001),
            worker: "writer".into(),
            path: "file".into(),
            base,
            value: Some(ManagedContent {
                content: Content {
                    hash: blake3::hash(b"").to_hex().to_string(),
                    size: 0,
                    manifest,
                },
                objects: vec![format!("crypt:{archive}/manifest.json")],
            }),
        }
    }
    #[test]
    fn repeated_retention_bounds_history_and_expires_offline_requests() {
        let mut state = Checkpoint::empty(&id(999));
        let offline = proposal(0, 900, None);
        let mut removed = 0;
        for n in 1..=120 {
            let p = proposal(
                state.serial,
                n,
                state.files.get("file").map(|v| v.id.clone()),
            );
            let (next, obsolete) = state.apply_batch(&[p], 2).unwrap();
            removed += obsolete.len();
            state = next;
            assert!(state.history.values().map(Vec::len).sum::<usize>() <= 2);
            assert_eq!(state.receipts.len(), 1);
        }
        assert_eq!(removed, 117);
        let latest = state.files["file"].id.clone();
        let (next, obsolete) = state
            .apply_batch(std::slice::from_ref(&offline), 2)
            .unwrap();
        assert_eq!(next.files["file"].id, latest);
        assert!(!next.receipts.contains(&offline.id));
        assert_eq!(obsolete, offline.value.unwrap().objects);
        let mut delete = proposal(0, 901, Some(id(1)));
        delete.value = None;
        let (next, _) = next.apply_batch(&[delete], 2).unwrap();
        assert_eq!(next.files["file"].id, latest);
    }
    #[test]
    fn concurrent_edits_preserve_conflicts_and_reject_mismatched_delete() {
        let state = Checkpoint::empty(&id(999));
        let (state, _) = state.apply_batch(&[proposal(0, 1, None)], 0).unwrap();
        let a = proposal(1, 2, Some(id(1)));
        let b = proposal(1, 3, Some(id(1)));
        let mut delete = proposal(1, 4, Some(id(1)));
        delete.value = None;
        let (next, _) = state.apply_batch(&[b, delete, a], 0).unwrap();
        assert_eq!(next.files.len(), 2);
        assert_eq!(next.files["file"].id, id(2));
        assert!(next.files.keys().any(|p| p.contains("conflict")));
        assert_eq!(next.receipts, [id(2), id(3), id(4)].into_iter().collect());
    }
    #[test]
    fn deletion_does_not_resurrect_and_empty_compaction_advances() {
        let (state, _) = Checkpoint::empty(&id(999))
            .apply_batch(&[proposal(0, 1, None)], 0)
            .unwrap();
        let mut deletion = proposal(1, 2, Some(id(1)));
        deletion.value = None;
        let (state, objects) = state.apply_batch(&[deletion], 0).unwrap();
        assert!(state.files.is_empty());
        assert_eq!(objects.len(), 1);
        let (state, _) = state.apply_batch(&[proposal(0, 3, None)], 0).unwrap();
        assert!(state.files.is_empty());
        assert!(state.receipts.is_empty());
        let (next, _) = state.compact(0).unwrap();
        assert_eq!(next.serial, state.serial + 1);
    }
    #[test]
    fn ownership_and_unowned_injection_fail_closed() {
        let state = Checkpoint::empty(&id(999));
        let mut p = proposal(0, 1, None);
        p.value.as_mut().unwrap().objects.clear();
        assert!(state.apply_batch(&[p], 0).is_err());
        let mut p = proposal(0, 1, None);
        p.value.as_mut().unwrap().objects[0] = "crypt:outside/manifest.json".into();
        assert!(state.apply_batch(&[p], 0).is_err());
        let mut p = proposal(0, 1, None);
        p.serial = 1;
        assert!(state.apply_batch(&[p], 0).is_err());
    }
    #[test]
    fn same_archive_import_protects_owned_metadata() {
        let p = proposal(0, 1, None);
        let (mut state, _) = Checkpoint::empty(&id(999)).apply_batch(&[p], 0).unwrap();
        let mut imported = state.files["file"].clone();
        imported.id = id(30);
        imported.value.objects.clear();
        state.files.insert("imported".into(), imported);
        let replacement = proposal(1, 2, Some(id(1)));
        let (next, objects) = state.apply_batch(&[replacement], 0).unwrap();
        assert!(objects.is_empty());
        assert!(next.files.contains_key("imported"));
    }
}
