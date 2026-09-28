//! Immutable, causally linked namespace events; no wall-clock conflict ordering.
use crate::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Content {
    pub hash: String,
    pub size: u64,
    pub manifest: Manifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    pub version: u32,
    pub worker: String,
    pub device: String,
    pub path: String,
    pub parents: Vec<String>,
    pub content: Option<Content>,
}

#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub event_id: String,
    pub event: Event,
}

fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

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
    pub(crate) fn id(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

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
fn overlaps(a: &str, b: &str) -> bool {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    a == b
        || a.strip_prefix(&b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|s| s.starts_with('/'))
}

fn conflict_path(path: &str, worker: &str, id: &str, attempt: usize) -> Result<String> {
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
    let annotation = format!(" (conflict-{worker}-{id}{suffix})");
    let stem = shortened(stem, 255 - annotation.len() - extension.len());
    let name = format!("{stem}{annotation}{extension}");
    component(&name)?;
    Ok(if directory.is_empty() {
        name
    } else {
        format!("{directory}/{name}")
    })
}

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
    let mut remaining: BTreeSet<_> = events.keys().cloned().collect();
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .filter(|id| events[*id].parents.iter().all(|p| !remaining.contains(p)))
            .cloned()
            .collect();
        if ready.is_empty() {
            bail!("namespace event ancestry cycle");
        }
        for id in ready {
            remaining.remove(&id);
        }
    }
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
    for (i, path) in reserved.iter().enumerate() {
        if reserved[i + 1..].iter().any(|other| overlaps(path, other)) {
            bail!("namespace case or file/directory collision: {path}");
        }
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
