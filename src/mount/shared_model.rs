//! Immutable, causally linked namespace events; no wall-clock conflict ordering.
//!
//! Defines the namespace [`Event`] (one revision or deletion of a path), its
//! validation and content id, the version-3 `reduce` projection and the
//! conflict/peer file naming shared with `peer_projection`.
use crate::prelude::*;

/// File content of a revision: what was uploaded and how to read it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Content {
    /// Hex BLAKE3 of the file bytes.
    pub hash: String,
    /// File size in bytes (must equal `manifest.original_size`).
    pub size: u64,
    /// Shard manifest of the uploaded archive.
    pub manifest: Manifest,
}

/// One immutable namespace event; its id is the BLAKE3 of its JSON
/// ([`Event::id`]). Published to the pool by pool sync and stored in
/// `Namespace::events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    /// Event format version (only 1 is valid).
    pub version: u32,
    /// Worker (PC) name that wrote the event.
    pub worker: String,
    /// Device id of the writing workspace.
    pub device: String,
    /// Namespace path (`/`-separated, portable components).
    pub path: String,
    /// Ids of the previous revisions of the same path (empty for a new file).
    pub parents: Vec<String>,
    /// New content; `None` records a deletion.
    pub content: Option<Content>,
}

/// A visible file: the event that currently provides it.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    /// Content id of the event.
    pub event_id: String,
    /// The event itself.
    pub event: Event,
}

/// Whether `value` is 64 lowercase hex digits (an event or content hash).
fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Checks one path component (or worker/device name) is portable: no
/// reserved Windows names or characters, no trailing dot/space, ≤ 255 bytes.
fn component(value: &str) -> Result<()> {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.ends_with(['.', ' '])
        || value.len() > 255
        || value
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        bail!("nonportable namespace component: {value:?}");
    }
    Ok(())
}

impl Event {
    /// Content id: hex BLAKE3 of the event's JSON serialization.
    pub(crate) fn id(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    /// Checks version, portable worker/device/path, unique well-formed parents,
    /// and a valid content manifest whose size matches.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported namespace event version");
        }
        component(&self.worker)?;
        component(&self.device)?;
        for part in self.path.split('/') {
            component(part)?;
        }
        let mut seen = BTreeSet::new();
        for parent in &self.parents {
            if !hash_valid(parent) || !seen.insert(parent) {
                bail!("invalid or duplicate event parent");
            }
        }
        if let Some(content) = &self.content {
            if !hash_valid(&content.hash) || content.size != content.manifest.original_size {
                bail!("invalid namespace content hash or size");
            }
            crate::manifest::validate_manifest(&content.manifest)?;
        }
        Ok(())
    }
}

// Equality and ancestor relationships are both collisions for materialized files.
/// Whether two paths collide case-insensitively: equal, or one is an
/// ancestor directory of the other.
pub(crate) fn overlaps(a: &str, b: &str) -> bool {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    a == b
        || a.strip_prefix(&b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|s| s.starts_with('/'))
}

/// Version-3 conflict name: `stem (conflict-<worker>-<id>[-n]).ext`.
pub(crate) fn conflict_path(path: &str, worker: &str, id: &str, attempt: usize) -> Result<String> {
    labelled_path(path, worker, id, attempt, false)
}
/// v6 peer conflict name: `stem_<worker>+a-<id>[-n].ext`, used by
/// `peer_projection`.
pub(crate) fn peer_path(path: &str, worker: &str, id: &str, attempt: usize) -> Result<String> {
    labelled_path(path, worker, id, attempt, true)
}
/// Builds a conflict (`peer` false) or peer (`peer` true) name for `path`,
/// shortening parts so the final component stays portable.
fn labelled_path(path: &str, worker: &str, id: &str, attempt: usize, peer: bool) -> Result<String> {
    let (directory, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
    let (stem, extension) = name
        .rsplit_once('.')
        .filter(|(s, _)| !s.is_empty())
        .map_or((name, String::new()), |(s, e)| (s, format!(".{e}")));
    let suffix = if attempt == 0 {
        String::new()
    } else {
        format!("-{attempt}")
    };
    fn shortened(value: &str, max: usize) -> &str {
        let mut end = value.len().min(max);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        &value[..end]
    }
    let worker = shortened(worker, 64);
    let extension = shortened(&extension, 32);
    let annotation = if peer {
        format!("_{worker}+a-{id}{suffix}")
    } else {
        format!(" (conflict-{worker}-{id}{suffix})")
    };
    let stem = shortened(stem, 255 - annotation.len() - extension.len());
    let name = format!("{stem}{annotation}{extension}");
    component(&name)?;
    Ok(if directory.is_empty() {
        name
    } else {
        format!("{directory}/{name}")
    })
}

/// Version-3 projection: validates the event DAG and paths, then places
/// each live head at its path; extra heads (or all heads when a deletion is
/// concurrent) get conflict names.
pub(crate) fn reduce(events: &BTreeMap<String, Event>) -> Result<BTreeMap<String, Resolved>> {
    let mut heads: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, event) in events {
        event.validate()?;
        if &event.id()? != id {
            bail!("namespace event identity mismatch");
        }
        heads
            .entry(event.path.clone())
            .or_default()
            .insert(id.clone());
        for parent in &event.parents {
            let predecessor = events
                .get(parent)
                .ok_or_else(|| anyhow!("missing namespace parent: {parent}"))?;
            if predecessor.path != event.path {
                bail!("namespace parent belongs to another path");
            }
        }
    }
    // Validate acyclicity explicitly rather than assuming content-addressed IDs imply it.
    // Kahn's algorithm: O(N log N) even for long edit chains (a layered scan
    // re-read every remaining event once per chain step).
    ensure_acyclic(events)?;
    for event in events.values() {
        for parent in &event.parents {
            heads.get_mut(&event.path).unwrap().remove(parent);
        }
    }
    // Deleted historical paths do not reserve a filesystem slot forever.
    // Their causal records remain for recreation/conflict handling.
    let reserved: Vec<_> = heads
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| events[id].content.is_some()))
        .map(|(path, _)| path.clone())
        .collect();
    let mut spellings = BTreeMap::new();
    for path in &reserved {
        let mut prefix = String::new();
        for part in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(previous) = spellings.insert(prefix.to_lowercase(), prefix.clone()) {
                if previous != prefix {
                    bail!("namespace directory case collision: {prefix}");
                }
            }
        }
    }
    if let Some(path) = first_overlap(&reserved) {
        bail!("namespace case or file/directory collision: {path}");
    }
    let mut result: BTreeMap<String, Resolved> = BTreeMap::new();
    for (path, ids) in heads {
        let has_delete = ids.iter().any(|id| events[id].content.is_none());
        let mut original_available = !has_delete;
        for id in ids {
            let event = &events[&id];
            if event.content.is_none() {
                continue;
            }
            let target = if original_available {
                original_available = false;
                path.clone()
            } else {
                let mut attempt = 0;
                loop {
                    let candidate = conflict_path(&path, &event.worker, &id, attempt)?;
                    if !reserved.iter().any(|p| overlaps(p, &candidate))
                        && !result.keys().any(|p| overlaps(p, &candidate))
                    {
                        break candidate;
                    }
                    attempt += 1;
                    if attempt > events.len() + 1 {
                        bail!("cannot allocate conflict filename");
                    }
                }
            };
            result.insert(
                target,
                Resolved {
                    event_id: id,
                    event: event.clone(),
                },
            );
        }
    }
    Ok(result)
}

/// Fails when the parent links (all present, checked by the caller) contain a cycle.
fn ensure_acyclic(events: &BTreeMap<String, Event>) -> Result<()> {
    let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut ready = vec![];
    for (id, event) in events {
        waiting.insert(id, event.parents.len());
        if event.parents.is_empty() {
            ready.push(id.as_str());
        }
        for parent in &event.parents {
            children.entry(parent).or_default().push(id);
        }
    }
    let mut done = 0usize;
    while let Some(id) = ready.pop() {
        done += 1;
        for child in children.get(id).into_iter().flatten() {
            let count = waiting.get_mut(child).expect("child is an event");
            *count -= 1;
            if *count == 0 {
                ready.push(child);
            }
        }
    }
    if done != events.len() {
        bail!("namespace event ancestry cycle");
    }
    Ok(())
}

/// A path of `reserved` that [`overlaps`] another one (case-folded equality or
/// ancestry), found in O(N log N) instead of comparing every pair. Lowercasing
/// maps `/` only to itself, so the folded ancestors of a path are exactly the
/// folded prefixes ending before one of its separators.
fn first_overlap(reserved: &[String]) -> Option<&str> {
    let mut folded = BTreeSet::new();
    for path in reserved {
        if !folded.insert(path.to_lowercase()) {
            return Some(path);
        }
    }
    reserved.iter().map(String::as_str).find(|path| {
        path.match_indices('/')
            .any(|(at, _)| folded.contains(&path[..at].to_lowercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(worker: &str, path: &str, parents: Vec<String>) -> Event {
        let manifest = Manifest {
            version: 2,
            archive_id: worker.into(),
            original_name: "empty".into(),
            original_size: 0,
            shard_size: 1024,
            created_unix: 0,
            content_root_blake3: crate::manifest::content_root_v2(0, 1024, &None, &[]),
            coding: None,
            shards: vec![],
        };
        Event {
            version: 1,
            worker: worker.into(),
            device: worker.into(),
            path: path.into(),
            parents,
            content: Some(Content {
                hash: blake3::hash(b"").to_hex().to_string(),
                size: 0,
                manifest,
            }),
        }
    }
    fn map(events: Vec<Event>) -> BTreeMap<String, Event> {
        events.into_iter().map(|e| (e.id().unwrap(), e)).collect()
    }
    #[test]
    fn sequential_and_concurrent_heads_preserve_content() {
        let base = put("a", "hello.txt", vec![]);
        let a = put("a", "hello.txt", vec![base.id().unwrap()]);
        let b = put("b", "hello.txt", vec![base.id().unwrap()]);
        assert_eq!(
            reduce(&map(vec![base.clone(), a.clone()])).unwrap().len(),
            1
        );
        let result = reduce(&map(vec![base, a, b])).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("hello.txt"));
        assert!(result.keys().any(|p| p.contains(" (conflict-")));
    }
    #[test]
    fn delete_edit_keeps_edit_as_conflict() {
        let base = put("a", "hello", vec![]);
        let edit = put("b", "hello", vec![base.id().unwrap()]);
        let mut delete = put("a", "hello", vec![base.id().unwrap()]);
        delete.content = None;
        let result = reduce(&map(vec![base, edit, delete])).unwrap();
        assert_eq!(result.len(), 1);
        assert!(!result.contains_key("hello"));
        assert!(result.keys().next().unwrap().contains("conflict-b-"));
    }
    #[test]
    fn missing_parents_and_path_collisions_fail_closed() {
        assert!(reduce(&map(vec![put("a", "a", vec!["0".repeat(64)])])).is_err());
        assert!(reduce(&map(vec![put("a", "a", vec![]), put("b", "A", vec![])])).is_err());
        assert!(reduce(&map(vec![put("a", "a", vec![]), put("b", "a/b", vec![])])).is_err());
        assert!(reduce(&map(vec![
            put("a", "Dir/a", vec![]),
            put("b", "dir/b", vec![])
        ]))
        .is_err());
    }
    #[test]
    fn resolution_descending_from_both_heads_removes_conflicts() {
        let a = put("a", "hello", vec![]);
        let b = put("b", "hello", vec![]);
        let resolved = put("c", "hello", vec![a.id().unwrap(), b.id().unwrap()]);
        let result = reduce(&map(vec![a, b, resolved])).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("hello"));
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;

    /// The pairwise check `first_overlap` replaced.
    fn pairwise(reserved: &[String]) -> bool {
        (0..reserved.len()).any(|i| {
            reserved[i + 1..]
                .iter()
                .any(|other| overlaps(&reserved[i], other))
        })
    }

    #[test]
    fn first_overlap_agrees_with_the_pairwise_check() {
        let cases: &[&[&str]] = &[
            &["a", "ab", "a.txt", "b/a"],
            &["A", "a"],
            &["a", "a/b"],
            &["A/b", "a"],
            &["ΑΣ/x", "ας"],
            &["İ/x", "i̇"],
            &["dir/x", "dir0", "dir.txt", "dir/y/z"],
            &["x/y/z", "X/Y"],
            &["x/y/z", "x/yz"],
        ];
        for case in cases {
            let mut reserved: Vec<String> = case.iter().map(|s| s.to_string()).collect();
            reserved.sort();
            assert_eq!(
                first_overlap(&reserved).is_some(),
                pairwise(&reserved),
                "{reserved:?}"
            );
        }
    }
}
