//! Normalized revision graph of one drive, built from its v6 events
//! (`source_v6`). Trash, versions and rollback only read this graph. Paths
//! are namespace paths (no leading `/`).
use crate::prelude::*;

/// Plaintext content of a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RevContent {
    /// Whole-file hash (`Content.hash`).
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rev {
    /// File identity: the event path.
    pub lineage: String,
    pub parents: Vec<String>,
    /// `None`: this revision deletes the file.
    pub content: Option<RevContent>,
    pub author: String,
    /// When the record reached the cloud (object ModTime), or now for local
    /// unpublished records. `None`: unknown (compacted away).
    pub time: Option<u64>,
}

/// Bytes of a revision when it is restored: the event content is
/// referenced again (no bytes move).
pub(crate) type Payload = crate::mount::history_bridge::Content;

pub(crate) struct History {
    pub revs: BTreeMap<String, Rev>,
    pub payloads: BTreeMap<String, Payload>,
    /// The drive's events; views are its own projection over them.
    pub events: BTreeMap<String, crate::mount::history_bridge::Event>,
    /// Deletion revisions purged from the trash (published history marks).
    pub purged: BTreeSet<String>,
    effective: BTreeMap<String, u64>,
    depth: BTreeMap<String, usize>,
}

impl History {
    pub(crate) fn new(
        revs: BTreeMap<String, Rev>,
        payloads: BTreeMap<String, Payload>,
        events: BTreeMap<String, crate::mount::history_bridge::Event>,
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
            revs,
            payloads,
            events,
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

    /// Visible files at `at` (`None`: now): path -> revision.
    pub(crate) fn view_at(&self, at: Option<u64>) -> Result<BTreeMap<String, String>> {
        let kept: BTreeMap<_, _> = self
            .events
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
    //! Small v6 histories for unit tests. Revisions get symbolic names
    //! (`"a1"`) that map to their real event ids.
    use super::*;
    use crate::drive_history::source_v6::fixture::{add, event};

    /// One event: path, parent names, text (`None`: deletion), worker, time.
    pub(crate) struct Spec {
        path: String,
        parents: Vec<String>,
        text: Option<String>,
        author: String,
        time: Option<u64>,
    }
    pub(crate) fn rev(
        path: &str,
        parents: &[&str],
        text: Option<&str>,
        author: &str,
        time: Option<u64>,
    ) -> Spec {
        Spec {
            path: path.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            text: text.map(Into::into),
            author: author.into(),
            time,
        }
    }

    /// A history plus the name <-> event id mapping.
    pub(crate) struct Fx {
        pub h: History,
        ids: BTreeMap<String, String>,
    }
    impl Fx {
        /// Event id of a revision name.
        pub(crate) fn id(&self, name: &str) -> String {
            self.ids[name].clone()
        }
        /// Revision name of an event id (other ids are returned unchanged).
        pub(crate) fn name<'a>(&'a self, id: &'a str) -> &'a str {
            self.ids
                .iter()
                .find(|(_, v)| v.as_str() == id)
                .map_or(id, |(k, _)| k.as_str())
        }
    }
    impl std::ops::Deref for Fx {
        type Target = History;
        fn deref(&self) -> &History {
            &self.h
        }
    }
    impl std::ops::DerefMut for Fx {
        fn deref_mut(&mut self) -> &mut History {
            &mut self.h
        }
    }

    /// Builds the history; parents must be listed before their children.
    pub(crate) fn history(revs: Vec<(&str, Spec)>) -> Fx {
        let mut events = BTreeMap::new();
        let mut ids: BTreeMap<String, String> = BTreeMap::new();
        let mut times = BTreeMap::new();
        for (name, spec) in revs {
            let parents: Vec<String> = spec.parents.iter().map(|p| ids[p].clone()).collect();
            let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
            let id = add(
                &mut events,
                event(&spec.author, &spec.path, &parents, spec.text.as_deref()),
            );
            if let Some(time) = spec.time {
                times.insert(id.clone(), time);
            }
            assert!(ids.insert(name.to_owned(), id).is_none(), "{name}");
        }
        let h = crate::drive_history::source_v6::build(
            events,
            &times,
            &BTreeSet::new(),
            0,
            BTreeSet::new(),
        )
        .unwrap();
        Fx { h, ids }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{history, rev};
    use super::*;

    #[test]
    fn effective_times_are_monotone_and_views_filter_by_time() {
        let h = history(vec![
            ("a1", rev("a.txt", &[], Some("one"), "pc", Some(100))),
            // Unknown own time inherits the parent's.
            ("a2", rev("a.txt", &["a1"], Some("two"), "pc", None)),
            ("a3", rev("a.txt", &["a2"], Some("three"), "pc", Some(300))),
            ("b1", rev("b.txt", &[], Some("b"), "pc", Some(200))),
        ]);
        assert_eq!(h.effective(&h.id("a2")), 100);
        let then = h.view_at(Some(150)).unwrap();
        assert_eq!(then.get("a.txt").map(|id| h.name(id)), Some("a2"));
        assert!(!then.contains_key("b.txt"));
        let now = h.view_at(None).unwrap();
        assert_eq!(h.name(&now["a.txt"]), "a3");
        assert_eq!(h.name(&now["b.txt"]), "b1");
        assert_eq!(h.last_content(&h.id("a3")), Some(h.id("a3")));
    }

    #[test]
    fn concurrent_heads_show_original_and_labelled_copies() {
        let h = history(vec![
            ("o", rev("a.txt", &[], Some("o"), "pc", Some(1))),
            ("x", rev("a.txt", &["o"], Some("x"), "PC-A", Some(2))),
            ("y", rev("a.txt", &["o"], Some("y"), "PC-B", Some(3))),
        ]);
        let view = h.view_at(None).unwrap();
        assert_eq!(h.name(&view["a.txt"]), "o");
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
