//! Normalized revision graph of one drive, built from v6 events
//! (`source_v6`) or v7 snapshots (`source_v7`). Trash, versions and rollback
//! only read this graph. Paths are namespace paths (no leading `/`).
use crate::prelude::*;

/// Plaintext content of a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RevContent {
    /// Whole-file hash (v6 `Content.hash`, v7 `content_hash`).
    pub hash: String,
    pub size: u64,
    /// Bytes are expected to still exist (v7: retained or not yet collected).
    pub restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rev {
    /// File identity: v6 event path, v7 stable file id.
    pub lineage: String,
    pub parents: Vec<String>,
    /// `None`: this revision deletes the file.
    pub content: Option<RevContent>,
    pub author: String,
    /// When the record reached the cloud (object ModTime), or now for local
    /// unpublished records. `None`: unknown (compacted away).
    pub time: Option<u64>,
}

/// How bytes of a revision are obtained when it is restored.
#[derive(Debug, Clone)]
pub(crate) enum Payload {
    /// v6: the event content is referenced again (no bytes move).
    Event(crate::mount::history_bridge::Content),
    /// v7: bytes are read from this manifest and uploaded as a new revision.
    Manifest(Manifest),
}

/// Paths of a v7 file over time: `(record time, path)` in causal order.
pub(crate) type Names = BTreeMap<String, Vec<(Option<u64>, String)>>;

pub(crate) enum Projection {
    /// v6: the drive's own projection over (time filtered) events.
    V6(BTreeMap<String, crate::mount::history_bridge::Event>),
    /// v7: per file names; projection mirrors the snapshot materialization.
    V7(Names),
}

pub(crate) struct History {
    pub mode: &'static str,
    pub revs: BTreeMap<String, Rev>,
    pub payloads: BTreeMap<String, Payload>,
    pub projection: Projection,
    /// Deletion revisions purged from the trash (published history marks).
    pub purged: BTreeSet<String>,
    effective: BTreeMap<String, u64>,
    depth: BTreeMap<String, usize>,
}

impl History {
    pub(crate) fn new(
        mode: &'static str,
        revs: BTreeMap<String, Rev>,
        payloads: BTreeMap<String, Payload>,
        projection: Projection,
        purged: BTreeSet<String>,
    ) -> Result<Self> {
        // Times only move forward along ancestry, so filtering by time keeps
        // every ancestor of a kept revision (an unknown time counts as old).
        let mut effective = BTreeMap::new();
        let mut depth = BTreeMap::new();
        let mut remaining: BTreeSet<_> = revs.keys().cloned().collect();
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .filter(|id| {
                    revs[*id]
                        .parents
                        .iter()
                        .all(|p| !remaining.contains(p) || !revs.contains_key(p))
                })
                .cloned()
                .collect();
            if ready.is_empty() {
                bail!("drive history ancestry cycle");
            }
            for id in ready {
                let rev = &revs[&id];
                let known: Vec<_> = rev
                    .parents
                    .iter()
                    .filter(|p| revs.contains_key(*p))
                    .collect();
                let time = known
                    .iter()
                    .map(|p| effective[*p])
                    .fold(rev.time.unwrap_or(0), u64::max);
                let level = known.iter().map(|p| depth[*p] + 1).max().unwrap_or(0);
                effective.insert(id.clone(), time);
                depth.insert(id.clone(), level);
                remaining.remove(&id);
            }
        }
        Ok(Self {
            mode,
            revs,
            payloads,
            projection,
            purged,
            effective,
            depth,
        })
    }

    pub(crate) fn effective(&self, id: &str) -> u64 {
        self.effective.get(id).copied().unwrap_or(0)
    }
    /// Newest first: effective time, then causal depth, then id.
    pub(crate) fn newest_first(&self, ids: &mut [String]) {
        ids.sort_by(|a, b| {
            (self.effective(b), self.depth.get(b), b).cmp(&(
                self.effective(a),
                self.depth.get(a),
                a,
            ))
        });
    }

    /// Head revisions (not a parent of any other) per lineage, at `at`.
    pub(crate) fn heads_at(&self, at: Option<u64>) -> BTreeMap<String, Vec<String>> {
        let kept = |id: &str| at.is_none_or(|t| self.effective(id) <= t);
        let referenced: BTreeSet<&String> = self
            .revs
            .iter()
            .filter(|(id, _)| kept(id))
            .flat_map(|(_, r)| r.parents.iter())
            .collect();
        let mut heads: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, rev) in &self.revs {
            if kept(id) && !referenced.contains(id) {
                heads
                    .entry(rev.lineage.clone())
                    .or_default()
                    .push(id.clone());
            }
        }
        heads
    }

    /// Nearest revision with content at or before `id` (the deleted bytes of
    /// a deletion; the revision itself when it has content).
    pub(crate) fn last_content(&self, id: &str) -> Option<String> {
        let mut level = vec![id.to_owned()];
        let mut seen = BTreeSet::new();
        while !level.is_empty() {
            let mut found: Vec<String> = level
                .iter()
                .filter(|id| self.revs.get(*id).is_some_and(|r| r.content.is_some()))
                .cloned()
                .collect();
            if !found.is_empty() {
                self.newest_first(&mut found);
                return found.into_iter().next();
            }
            let mut next = Vec::new();
            for id in level {
                if seen.insert(id.clone()) {
                    if let Some(rev) = self.revs.get(&id) {
                        next.extend(rev.parents.iter().cloned());
                    }
                }
            }
            level = next;
        }
        None
    }

    /// Every ancestor of `id` (excluding itself).
    pub(crate) fn ancestors(&self, id: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut pending: Vec<String> = self
            .revs
            .get(id)
            .map(|r| r.parents.clone())
            .unwrap_or_default();
        while let Some(id) = pending.pop() {
            if let Some(rev) = self.revs.get(&id) {
                if found.insert(id) {
                    pending.extend(rev.parents.iter().cloned());
                }
            }
        }
        found
    }

    /// Path of a lineage at `at` (`None`: now).
    pub(crate) fn lineage_path(&self, lineage: &str, at: Option<u64>) -> String {
        match &self.projection {
            Projection::V6(_) => lineage.to_owned(),
            Projection::V7(names) => names
                .get(lineage)
                .and_then(|list| {
                    list.iter()
                        .rev()
                        .find(|(time, _)| at.is_none_or(|t| time.unwrap_or(0) <= t))
                        .or_else(|| list.first())
                })
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| lineage.to_owned()),
        }
    }

    /// Visible files at `at` (`None`: now): path -> revision.
    pub(crate) fn view_at(&self, at: Option<u64>) -> Result<BTreeMap<String, String>> {
        match &self.projection {
            Projection::V6(events) => {
                let kept: BTreeMap<_, _> = events
                    .iter()
                    .filter(|(id, _)| at.is_none_or(|t| self.effective(id) <= t))
                    .map(|(id, e)| (id.clone(), e.clone()))
                    .collect();
                Ok(crate::mount::peer_projection::project(&kept)?
                    .files
                    .into_iter()
                    .filter(|(_, r)| r.event.content.is_some())
                    .map(|(path, r)| (path, r.event_id))
                    .collect())
            }
            Projection::V7(_) => self.view_v7(at),
        }
    }

    /// Mirrors the v7 materialization: one head at its path; with several
    /// heads the common original keeps the path and heads get labelled copies.
    fn view_v7(&self, at: Option<u64>) -> Result<BTreeMap<String, String>> {
        let mut view = BTreeMap::new();
        for (lineage, heads) in self.heads_at(at) {
            let path = self.lineage_path(&lineage, at);
            let live: Vec<_> = heads
                .iter()
                .filter(|id| self.revs[*id].content.is_some())
                .collect();
            if heads.len() == 1 {
                if let Some(id) = live.first() {
                    view.insert(path, (*id).clone());
                }
                continue;
            }
            if live.is_empty() {
                continue;
            }
            let mut common: Option<BTreeSet<String>> = None;
            for head in &heads {
                let mut set = self.ancestors(head);
                set.insert(head.clone());
                common = Some(match common {
                    None => set,
                    Some(c) => c.intersection(&set).cloned().collect(),
                });
            }
            let common = common.unwrap_or_default();
            let older: BTreeSet<_> = common.iter().flat_map(|id| self.ancestors(id)).collect();
            let originals: Vec<_> = common
                .difference(&older)
                .filter(|id| self.revs[*id].content.is_some())
                .collect();
            if originals.len() == 1 {
                view.insert(path.clone(), originals[0].clone());
            }
            for id in live {
                let rev = &self.revs[id];
                let revision = id.rsplit(':').next().unwrap_or(id);
                let label =
                    crate::mount::history_bridge::peer_path(&path, &rev.author, revision, 0)?;
                view.insert(label, id.clone());
            }
        }
        Ok(view)
    }

    /// Lineages whose every head deletes the file.
    pub(crate) fn deleted_lineages(&self) -> BTreeMap<String, Vec<String>> {
        self.heads_at(None)
            .into_iter()
            .filter(|(_, heads)| heads.iter().all(|id| self.revs[id].content.is_none()))
            .collect()
    }
}

/// Case-insensitive equality or file/folder nesting of two paths.
pub(crate) fn collides(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    a == b
        || a.strip_prefix(&b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|s| s.starts_with('/'))
}

/// `/Docs/a.txt` -> `Docs/a.txt` (validated namespace path; `/` -> empty).
pub(crate) fn namespace_path(display: &str) -> Result<String> {
    let path = display.trim_matches('/').replace('\\', "/");
    if !path.is_empty() {
        crate::mount::history_bridge::valid_path(&path)?;
    }
    Ok(path)
}
pub(crate) fn display_path(path: &str) -> String {
    format!("/{path}")
}
/// Whether `path` is `scope` or inside it (`scope` empty: whole drive).
pub(crate) fn in_scope(path: &str, scope: &str) -> bool {
    scope.is_empty()
        || path.eq_ignore_ascii_case(scope)
        || path
            .to_lowercase()
            .strip_prefix(&scope.to_lowercase())
            .is_some_and(|s| s.starts_with('/'))
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Small v7-style graphs for unit tests (no payloads needed).
    use super::*;
    pub(crate) fn rev(
        lineage: &str,
        parents: &[&str],
        content: Option<&str>,
        author: &str,
        time: Option<u64>,
    ) -> Rev {
        Rev {
            lineage: lineage.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            content: content.map(|text| RevContent {
                hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
                size: text.len() as u64,
                restorable: true,
            }),
            author: author.into(),
            time,
        }
    }
    pub(crate) fn history(revs: Vec<(&str, Rev)>, names: &[(&str, &str)]) -> History {
        let names = names
            .iter()
            .map(|(lineage, path)| (lineage.to_string(), vec![(None, path.to_string())]))
            .collect();
        History::new(
            "v7",
            revs.into_iter()
                .map(|(id, r)| (id.to_string(), r))
                .collect(),
            BTreeMap::new(),
            Projection::V7(names),
            BTreeSet::new(),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{history, rev};
    use super::*;

    #[test]
    fn effective_times_are_monotone_and_views_filter_by_time() {
        let h = history(
            vec![
                ("a1", rev("A", &[], Some("one"), "pc", Some(100))),
                // Unknown own time inherits the parent's.
                ("a2", rev("A", &["a1"], Some("two"), "pc", None)),
                ("a3", rev("A", &["a2"], Some("three"), "pc", Some(300))),
                ("b1", rev("B", &[], Some("b"), "pc", Some(200))),
            ],
            &[("A", "a.txt"), ("B", "b.txt")],
        );
        assert_eq!(h.effective("a2"), 100);
        let then = h.view_at(Some(150)).unwrap();
        assert_eq!(then.get("a.txt").map(String::as_str), Some("a2"));
        assert!(!then.contains_key("b.txt"));
        let now = h.view_at(None).unwrap();
        assert_eq!(now["a.txt"], "a3");
        assert_eq!(now["b.txt"], "b1");
        assert_eq!(h.last_content("a3").as_deref(), Some("a3"));
    }

    #[test]
    fn concurrent_heads_show_original_and_labelled_copies() {
        let h = history(
            vec![
                ("o", rev("A", &[], Some("o"), "pc", Some(1))),
                ("x", rev("A", &["o"], Some("x"), "PC-A", Some(2))),
                ("y", rev("A", &["o"], Some("y"), "PC-B", Some(3))),
            ],
            &[("A", "a.txt")],
        );
        let view = h.view_at(None).unwrap();
        assert_eq!(view["a.txt"], "o");
        assert_eq!(view.len(), 3);
        assert!(view.keys().any(|p| p.contains("PC-A")));
        assert!(view.keys().any(|p| p.contains("PC-B")));
    }

    #[test]
    fn paths_scope_and_collisions() {
        assert_eq!(namespace_path("/Docs/a.txt").unwrap(), "Docs/a.txt");
        assert_eq!(namespace_path("/").unwrap(), "");
        assert!(namespace_path("/a/../b").is_err());
        assert!(in_scope("Docs/a", "docs"));
        assert!(!in_scope("Docsx/a", "Docs"));
        assert!(collides("Docs", "docs/a.txt"));
        assert!(!collides("Doc", "Docs"));
    }
}
