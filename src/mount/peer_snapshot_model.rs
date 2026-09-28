//! V7 private-ownership snapshot model. Metadata is permanent causal evidence;
//! GC removes only exact owned payload keys after positive successor validation.
//! Transport must verify all plaintext hashes/bytes and durable publication.
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Policy {
    pub genesis_id: String,
    pub history_limit: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Revision {
    pub parents: BTreeSet<String>,
    pub content_hash: Option<String>,
    pub size: u64,
    pub worker: String,
    pub device: String,
}
impl Revision {
    pub(crate) fn id(&self) -> Result<String> {
        digest(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub version: u32,
    pub file_id: String,
    /// Random unique ownership nonce, NOT the hash of this snapshot.
    pub owner_id: String,
    pub policy: Policy,
    pub parents: BTreeSet<String>,
    pub covered: BTreeSet<String>,
    pub revisions: BTreeMap<String, Revision>,
    pub heads: BTreeSet<String>,
    pub payloads: BTreeMap<String, Manifest>,
    /// Required ancestor/history bytes absent in all direct parents. Never heads.
    pub unavailable: BTreeSet<String>,
}
impl Snapshot {
    pub(crate) fn id(&self) -> Result<String> {
        digest(self)
    }
    pub(crate) fn owned_objects(&self) -> BTreeSet<String> {
        self.payloads
            .values()
            .flat_map(|m| m.shards.iter().map(|s| s.object.clone()))
            .collect()
    }
}
#[derive(Debug, Clone)]
pub(crate) struct PayloadSource {
    pub snapshot_id: String,
    pub manifest: Manifest,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct Merged {
    pub revisions: BTreeMap<String, Revision>,
    pub heads: BTreeSet<String>,
    pub originals: BTreeSet<String>,
    /// N nearest content ancestors per maximal lineage; conflicts are extra.
    pub history: BTreeSet<String>,
    pub required: BTreeSet<String>,
    pub unavailable: BTreeSet<String>,
    pub payloads: BTreeMap<String, PayloadSource>,
}
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    pub frontier: BTreeSet<String>,
    pub merged: BTreeMap<String, Merged>,
    /// Exact retired snapshot -> positive dominating successor proof.
    pub gc: BTreeMap<String, String>,
}
fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn owner_archive(owner: &str) -> String {
    format!("peer-v7-{owner}")
}
fn validate_policy(policy: &Policy) -> Result<()> {
    if !hash(&policy.genesis_id) || policy.history_limit > 10_000 {
        bail!("invalid snapshot policy genesis/history limit");
    }
    Ok(())
}
fn ancestors(id: &str, revisions: &BTreeMap<String, Revision>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![id.to_owned()];
    while let Some(id) = pending.pop() {
        if found.insert(id.clone()) {
            pending.extend(revisions[&id].parents.iter().cloned());
        }
    }
    found
}
fn validate_revisions(revisions: &BTreeMap<String, Revision>) -> Result<()> {
    if revisions.is_empty() {
        bail!("empty snapshot content graph");
    }
    let mut done = BTreeSet::new();
    for (id, r) in revisions {
        if r.id()? != *id
            || r.content_hash.as_ref().is_some_and(|h| !hash(h))
            || (r.content_hash.is_none() && r.size != 0)
        {
            bail!("invalid semantic revision identity/content");
        }
        // Reuse established portable label validation, not raw path rendering.
        super::namespace::valid_path(&r.worker)?;
        super::namespace::valid_path(&r.device)?;
        if r.worker.contains('/') || r.device.contains('/') {
            bail!("invalid revision label");
        }
        for p in &r.parents {
            if !revisions.contains_key(p) {
                bail!("missing revision ancestry");
            }
        }
    }
    while done.len() < revisions.len() {
        let next: Vec<_> = revisions
            .iter()
            .filter(|(id, r)| !done.contains(*id) && r.parents.iter().all(|p| done.contains(p)))
            .map(|(id, _)| id.clone())
            .collect();
        if next.is_empty() {
            bail!("cyclic revision ancestry");
        }
        done.extend(next);
    }
    Ok(())
}
fn select(revisions: BTreeMap<String, Revision>, limit: usize) -> Result<Merged> {
    validate_revisions(&revisions)?;
    let referenced: BTreeSet<_> = revisions
        .values()
        .flat_map(|r| r.parents.iter().cloned())
        .collect();
    let heads: BTreeSet<_> = revisions
        .keys()
        .filter(|id| !referenced.contains(*id))
        .cloned()
        .collect();
    let mut originals = BTreeSet::new();
    if heads.len() > 1 && heads.iter().any(|id| revisions[id].content_hash.is_some()) {
        let mut common = ancestors(heads.first().unwrap(), &revisions);
        for h in heads.iter().skip(1) {
            let a = ancestors(h, &revisions);
            common.retain(|id| a.contains(id));
        }
        let mut older = BTreeSet::new();
        for id in &common {
            let mut a = ancestors(id, &revisions);
            a.remove(id);
            older.extend(a);
        }
        originals = common
            .difference(&older)
            .filter(|id| revisions[*id].content_hash.is_some())
            .cloned()
            .collect();
    }
    let mut history = BTreeSet::new();
    for h in &heads {
        let mut seen = BTreeSet::new();
        let mut level = revisions[h].parents.clone();
        let mut count = 0;
        while !level.is_empty() && count < limit {
            let mut next = BTreeSet::new();
            for id in level {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if revisions[&id].content_hash.is_some() && count < limit {
                    history.insert(id.clone());
                    count += 1;
                }
                next.extend(revisions[&id].parents.iter().cloned());
            }
            level = next;
        }
    }
    let mut required: BTreeSet<_> = heads
        .iter()
        .filter(|id| revisions[*id].content_hash.is_some())
        .cloned()
        .collect();
    required.extend(originals.iter().cloned());
    required.extend(history.iter().cloned());
    Ok(Merged {
        revisions,
        heads,
        originals,
        history,
        unavailable: required.clone(),
        required,
        payloads: BTreeMap::new(),
    })
}
fn parent_merge(
    parents: &BTreeSet<String>,
    all: &BTreeMap<String, Snapshot>,
    file_id: &str,
    policy: &Policy,
) -> Result<Merged> {
    let mut revisions = BTreeMap::new();
    let mut payloads = BTreeMap::new();
    for id in parents {
        let s = all.get(id).context("missing snapshot parent")?;
        if s.file_id != file_id || &s.policy != policy {
            bail!("snapshot file/policy genesis mismatch");
        }
        for (id, r) in &s.revisions {
            if revisions.get(id).is_some_and(|old| old != r) {
                bail!("revision identity collision");
            }
            revisions.insert(id.clone(), r.clone());
        }
        for (revision, manifest) in &s.payloads {
            payloads
                .entry(revision.clone())
                .or_insert_with(|| PayloadSource {
                    snapshot_id: id.clone(),
                    manifest: manifest.clone(),
                });
        }
    }
    if revisions.is_empty() {
        return Ok(Merged::default());
    }
    let mut m = select(revisions, policy.history_limit)?;
    m.payloads = payloads;
    m.unavailable = m
        .required
        .iter()
        .filter(|id| !m.payloads.contains_key(*id))
        .cloned()
        .collect();
    Ok(m)
}
/// Plan bytes BEFORE uploading. Newly introduced revisions require direct-base
/// concurrency anchors; compaction with no new revisions may drop these anchors.
pub(crate) fn prepare(
    file_id: &str,
    policy: &Policy,
    parents: &BTreeSet<String>,
    all: &BTreeMap<String, Snapshot>,
    new_revisions: &BTreeMap<String, Revision>,
) -> Result<Merged> {
    validate_policy(policy)?;
    if !hash(file_id) {
        bail!("invalid stable file identity");
    }
    let old = parent_merge(parents, all, file_id, policy)?;
    let mut revisions = old.revisions.clone();
    for (id, r) in new_revisions {
        if revisions.get(id).is_some_and(|old| old != r) {
            bail!("revision identity collision");
        }
        revisions.insert(id.clone(), r.clone());
    }
    let mut m = select(revisions, policy.history_limit)?;
    for (id, r) in new_revisions {
        if !old.revisions.contains_key(id) {
            m.required.extend(
                r.parents
                    .iter()
                    .filter(|p| m.revisions[*p].content_hash.is_some())
                    .cloned(),
            );
        }
    }
    m.payloads = old.payloads;
    m.unavailable = m
        .required
        .iter()
        .filter(|id| !m.payloads.contains_key(*id))
        .cloned()
        .collect();
    Ok(m)
}
pub(crate) fn build(
    file_id: String,
    owner_id: String,
    policy: Policy,
    parents: BTreeSet<String>,
    all: &BTreeMap<String, Snapshot>,
    new_revisions: BTreeMap<String, Revision>,
    payloads: BTreeMap<String, Manifest>,
) -> Result<Snapshot> {
    // Validate the entire existing metadata set before authorizing domination.
    analyze(all, &policy)?;
    let m = prepare(&file_id, &policy, &parents, all, &new_revisions)?;
    let mut covered = parents.clone();
    for p in &parents {
        covered.extend(all[p].covered.iter().cloned());
    }
    let unavailable = m
        .required
        .iter()
        .filter(|id| !payloads.contains_key(*id))
        .cloned()
        .collect();
    let s = Snapshot {
        version: 7,
        file_id,
        owner_id,
        policy,
        parents,
        covered,
        revisions: m.revisions,
        heads: m.heads,
        payloads,
        unavailable,
    };
    let mut check = all.clone();
    check.insert(s.id()?, s.clone());
    analyze(&check, &s.policy)?;
    Ok(s)
}
fn validate_snapshot(
    s: &Snapshot,
    all: &BTreeMap<String, Snapshot>,
    policy: &Policy,
) -> Result<()> {
    if s.version != 7 || !hash(&s.file_id) || !hash(&s.owner_id) || &s.policy != policy {
        bail!("snapshot version/file/policy genesis mismatch");
    }
    let prior = parent_merge(&s.parents, all, &s.file_id, policy)?;
    for (id, r) in &prior.revisions {
        if s.revisions.get(id) != Some(r) {
            bail!("snapshot omitted/changed covered revision evidence");
        }
    }
    let new = s
        .revisions
        .iter()
        .filter(|(id, _)| !prior.revisions.contains_key(*id))
        .map(|(id, r)| (id.clone(), r.clone()))
        .collect();
    let m = prepare(&s.file_id, policy, &s.parents, all, &new)?;
    if m.heads != s.heads {
        bail!("snapshot head set mismatch");
    }
    let mut covered = s.parents.clone();
    for p in &s.parents {
        covered.extend(all[p].covered.iter().cloned());
    }
    if covered != s.covered {
        bail!("snapshot causal coverage is not proven by parents");
    }
    let absent: BTreeSet<_> = m
        .required
        .iter()
        .filter(|id| !s.payloads.contains_key(*id))
        .cloned()
        .collect();
    if absent != s.unavailable
        || absent
            .iter()
            .any(|id| m.heads.contains(id) || prior.payloads.contains_key(id))
    {
        bail!("snapshot omitted available required bytes or a live candidate");
    }
    let archive = owner_archive(&s.owner_id);
    let mut objects = BTreeMap::new();
    for (id, manifest) in &s.payloads {
        if !m.required.contains(id) {
            bail!("snapshot retains payload outside history/conflict/anchor closure");
        }
        let r = s
            .revisions
            .get(id)
            .context("payload without semantic revision")?;
        if r.content_hash.is_none() || manifest.original_size != r.size {
            bail!("snapshot payload semantic size/type mismatch");
        }
        crate::manifest::validate_manifest(manifest)?;
        if manifest.archive_id != archive
            && !manifest.archive_id.starts_with(&format!("{archive}/"))
        {
            bail!("payload manifest belongs to another owner");
        }
        if manifest
            .archive_id
            .split('/')
            .any(|p| matches!(p, "" | "." | ".."))
            || manifest.archive_id.contains('\\')
        {
            bail!("invalid owned manifest key");
        }
        for shard in &manifest.shards {
            let (_, remote_path) = shard
                .remote
                .split_once(':')
                .context("owned payload requires remote address")?;
            if remote_path.split('/').any(|p| matches!(p, "." | "..")) || remote_path.contains('\\')
            {
                bail!("invalid ownership remote root");
            }
            let root = crate::utils::remote_join(&shard.remote, &format!("{archive}/"));
            let key = shard
                .object
                .strip_prefix(&root)
                .context("snapshot references another owner's object")?;
            if key.is_empty()
                || key.split('/').any(|p| matches!(p, "" | "." | ".."))
                || key.contains('\\')
            {
                bail!("invalid owned object key");
            }
            let identity = (shard.size, shard.blake3.clone());
            if objects
                .get(&shard.object)
                .is_some_and(|old| old != &identity)
            {
                bail!("conflicting owned object descriptors");
            }
            objects.insert(shard.object.clone(), identity);
        }
    }
    Ok(())
}
pub(crate) fn analyze(all: &BTreeMap<String, Snapshot>, policy: &Policy) -> Result<Analysis> {
    validate_policy(policy)?;
    let mut done = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut object_owners = BTreeMap::new();
    for (id, s) in all {
        if s.id()? != *id || !owners.insert(s.owner_id.clone()) {
            bail!("snapshot identity/ownership nonce reused");
        }
        for object in s.owned_objects() {
            if object_owners.insert(object, s.owner_id.clone()).is_some() {
                bail!("physical object shared across snapshot owners");
            }
        }
    }
    while done.len() < all.len() {
        let next: Vec<_> = all
            .iter()
            .filter(|(id, s)| !done.contains(*id) && s.parents.iter().all(|p| done.contains(p)))
            .map(|(id, _)| id.clone())
            .collect();
        if next.is_empty() {
            bail!("missing/cyclic snapshot causal ancestry");
        }
        for id in next {
            validate_snapshot(&all[&id], all, policy)?;
            done.insert(id);
        }
    }
    let mut result = Analysis::default();
    for (id, s) in all {
        for old in &s.covered {
            result.gc.entry(old.clone()).or_insert_with(|| id.clone());
        }
    }
    result.frontier = all
        .keys()
        .filter(|id| !result.gc.contains_key(*id))
        .cloned()
        .collect();
    let files: BTreeSet<_> = result
        .frontier
        .iter()
        .map(|id| all[id].file_id.clone())
        .collect();
    for file in files {
        let parents = result
            .frontier
            .iter()
            .filter(|id| all[*id].file_id == file)
            .cloned()
            .collect();
        result
            .merged
            .insert(file.clone(), parent_merge(&parents, all, &file, policy)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token(n: u8) -> String {
        format!("{n:064x}")
    }
    fn policy(n: usize) -> Policy {
        Policy {
            genesis_id: token(1),
            history_limit: n,
        }
    }
    fn revision(parents: &[String], text: &str) -> Revision {
        Revision {
            parents: parents.iter().cloned().collect(),
            content_hash: Some(blake3::hash(text.as_bytes()).to_hex().to_string()),
            size: text.len() as u64,
            worker: "pc".into(),
            device: "device".into(),
        }
    }
    fn payload(owner: &str, id: &str, r: &Revision) -> Manifest {
        let archive = owner_archive(owner);
        let shards = vec![Shard {
            index: 0,
            offset: 0,
            size: r.size,
            remote: "crypt:pool".into(),
            object: format!("crypt:pool/{archive}/{id}/data"),
            blake3: r.content_hash.clone().unwrap(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        }];
        Manifest {
            version: 2,
            archive_id: format!("{archive}/{id}"),
            original_name: "file".into(),
            original_size: r.size,
            shard_size: r.size.max(1),
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(
                r.size,
                r.size.max(1),
                &None,
                &shards,
            ),
            coding: None,
            shards,
        }
    }
    fn add(
        all: &mut BTreeMap<String, Snapshot>,
        p: &Policy,
        owner: u8,
        parents: &[String],
        new: Vec<Revision>,
    ) -> String {
        let parents = parents.iter().cloned().collect();
        let new = new.into_iter().map(|r| (r.id().unwrap(), r)).collect();
        let m = prepare(&token(2), p, &parents, all, &new).unwrap();
        let payloads = m
            .required
            .iter()
            .map(|id| (id.clone(), payload(&token(owner), id, &m.revisions[id])))
            .collect();
        let s = build(
            token(2),
            token(owner),
            p.clone(),
            parents,
            all,
            new,
            payloads,
        )
        .unwrap();
        let id = s.id().unwrap();
        all.insert(id.clone(), s);
        id
    }
    #[test]
    fn private_successor_is_positive_gc_proof_and_zero_history_compacts_anchor() {
        let p = policy(0);
        let mut all = BTreeMap::new();
        let o = revision(&[], "original");
        let oid = o.id().unwrap();
        let a = add(&mut all, &p, 10, &[], vec![o]);
        let edit = revision(&[oid.clone()], "edit");
        let eid = edit.id().unwrap();
        let b = add(&mut all, &p, 11, &[a.clone()], vec![edit]);
        assert!(all[&b].payloads.contains_key(&oid)); // transient fork anchor
        let c = add(&mut all, &p, 12, &[b.clone()], vec![]);
        assert_eq!(
            all[&c].payloads.keys().cloned().collect::<Vec<_>>(),
            vec![eid]
        );
        let analysis = analyze(&all, &p).unwrap();
        assert!(analysis.gc.contains_key(&a));
        assert!(analysis.gc.contains_key(&b));
        assert!(!analysis.gc.contains_key(&c));
        assert!(all[&a]
            .owned_objects()
            .is_disjoint(&all[&c].owned_objects()));
    }
    #[test]
    fn unseen_sibling_remains_live_and_join_preserves_original_and_both_candidates() {
        let p = policy(0);
        let mut all = BTreeMap::new();
        let o = revision(&[], "original");
        let oid = o.id().unwrap();
        let root = add(&mut all, &p, 10, &[], vec![o]);
        let a = revision(&[oid.clone()], "A");
        let aid = a.id().unwrap();
        let left = add(&mut all, &p, 11, &[root.clone()], vec![a]);
        let compact = add(&mut all, &p, 12, &[left], vec![]);
        let b = revision(&[oid.clone()], "B");
        let bid = b.id().unwrap();
        let right = add(&mut all, &p, 13, &[root], vec![b]);
        let m = analyze(&all, &p).unwrap();
        assert_eq!(m.frontier.len(), 2);
        assert!(!m.gc.contains_key(&right));
        let projected = &m.merged[&token(2)];
        assert_eq!(projected.originals, BTreeSet::from([oid.clone()]));
        assert!(projected.unavailable.is_empty());
        let joined = add(&mut all, &p, 14, &[compact, right], vec![]);
        assert_eq!(
            all[&joined]
                .payloads
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([oid, aid, bid])
        );
    }
    #[test]
    fn late_multigeneration_fork_marks_original_unavailable_without_losing_heads() {
        let p = policy(0);
        let mut all = BTreeMap::new();
        let o = revision(&[], "O");
        let oid = o.id().unwrap();
        let root = add(&mut all, &p, 10, &[], vec![o]);
        let mut leaves = vec![];
        for (n, text) in [(20, "A"), (30, "B")] {
            let first = revision(&[oid.clone()], text);
            let fid = first.id().unwrap();
            let s = add(&mut all, &p, n, &[root.clone()], vec![first]);
            let second = revision(&[fid], &format!("{text}2"));
            let s = add(&mut all, &p, n + 1, &[s], vec![second]);
            leaves.push(add(&mut all, &p, n + 2, &[s], vec![]));
        }
        let parents = leaves.into_iter().collect();
        let new = BTreeMap::new();
        let m = prepare(&token(2), &p, &parents, &all, &new).unwrap();
        assert!(m.unavailable.contains(&oid));
        let payloads = m
            .required
            .iter()
            .filter(|id| m.payloads.contains_key(*id))
            .map(|id| (id.clone(), payload(&token(40), id, &m.revisions[id])))
            .collect();
        let joined = build(token(2), token(40), p.clone(), parents, &all, new, payloads).unwrap();
        assert_eq!(joined.heads.len(), 2);
        assert!(joined.unavailable.contains(&oid));
    }
    #[test]
    fn history_policy_private_ownership_and_coverage_are_enforced() {
        let p = policy(1);
        let mut all = BTreeMap::new();
        let o = revision(&[], "O");
        let oid = o.id().unwrap();
        let root = add(&mut all, &p, 10, &[], vec![o]);
        let a = revision(&[oid], "A");
        let aid = a.id().unwrap();
        let next = add(&mut all, &p, 11, &[root.clone()], vec![a]);
        let b = revision(&[aid.clone()], "B");
        let next = add(&mut all, &p, 12, &[next], vec![b]);
        let compact = add(&mut all, &p, 13, &[next], vec![]);
        assert_eq!(all[&compact].payloads.len(), 2);
        assert!(all[&compact].payloads.contains_key(&aid));
        assert!(analyze(&all, &policy(0)).is_err());
        let mut bad = all[&compact].clone();
        bad.covered.clear();
        let mut altered = all.clone();
        altered.remove(&compact);
        altered.insert(bad.id().unwrap(), bad);
        assert!(analyze(&altered, &p).is_err());
        let mut bad = all[&compact].clone();
        bad.payloads.insert(
            aid.clone(),
            all[&root].payloads.values().next().unwrap().clone(),
        );
        let mut altered = all.clone();
        altered.remove(&compact);
        altered.insert(bad.id().unwrap(), bad);
        assert!(analyze(&altered, &p).is_err());
        let mut bad = all[&compact].clone();
        bad.payloads.remove(&aid);
        bad.unavailable.insert(aid);
        let mut altered = all;
        altered.remove(&compact);
        altered.insert(bad.id().unwrap(), bad);
        assert!(analyze(&altered, &p).is_err());
    }

    #[test]
    fn delete_edit_and_resolution_racing_unseen_branch_preserve_content() {
        let p = policy(0);
        let mut all = BTreeMap::new();
        let o = revision(&[], "O");
        let oid = o.id().unwrap();
        let root = add(&mut all, &p, 10, &[], vec![o]);
        let mut deletion = revision(&[oid.clone()], "");
        deletion.content_hash = None;
        deletion.size = 0;
        let did = deletion.id().unwrap();
        let del = add(&mut all, &p, 11, &[root.clone()], vec![deletion]);
        let edit = revision(&[oid.clone()], "A");
        let aid = edit.id().unwrap();
        let a = add(&mut all, &p, 12, &[root.clone()], vec![edit]);
        let merged = analyze(&all, &p).unwrap();
        let m = &merged.merged[&token(2)];
        assert_eq!(m.heads, BTreeSet::from([did.clone(), aid.clone()]));
        assert!(m.originals.contains(&oid));
        assert!(!m.required.contains(&did));
        let resolution = revision(&[did, aid], "resolved");
        let rid = resolution.id().unwrap();
        let resolved = add(&mut all, &p, 13, &[del, a], vec![resolution]);
        let late = revision(&[oid.clone()], "late");
        let lid = late.id().unwrap();
        let late_snapshot = add(&mut all, &p, 14, &[root], vec![late]);
        let state = analyze(&all, &p).unwrap();
        assert_eq!(state.frontier, BTreeSet::from([resolved, late_snapshot]));
        let m = &state.merged[&token(2)];
        assert_eq!(m.heads, BTreeSet::from([rid, lid]));
        assert_eq!(m.originals, BTreeSet::from([oid]));
        assert!(m.unavailable.is_empty());
    }

    #[test]
    fn absence_never_proves_retirement_and_metadata_ancestry_is_mandatory() {
        let p = policy(0);
        let mut all = BTreeMap::new();
        let root = add(&mut all, &p, 10, &[], vec![revision(&[], "O")]);
        assert!(analyze(&all, &p).unwrap().gc.is_empty());
        let child = add(&mut all, &p, 11, &[root.clone()], vec![]);
        all.remove(&root);
        assert!(analyze(&all, &p).is_err());
        assert!(all.contains_key(&child));
    }
}
