//! Versions of one file: every revision of its lineage (including deletions
//! and concurrent conflict branches), newest first. v6 lineages are paths, so
//! a moved file also shows the history of the path it was moved from (same
//! bytes at the start of the new path, deleted at the old one).
use super::graph::{display_path, History};
use super::model::{VersionEntry, VersionKind};
use super::restore::{unique_name, Action};
use crate::prelude::*;

/// Lineages of `path`: the visible file there (or a conflict copy of it),
/// else a deleted file last named `path`, plus v6 move predecessors.
pub(crate) fn lineages(history: &History, path: &str) -> Result<Vec<String>> {
    let current = history.view_at(None)?;
    let mut result = Vec::new();
    if let Some(id) = current.get(path) {
        result.push(history.revs[id].lineage.clone());
    } else {
        let mut deleted: Vec<_> = history
            .deleted_lineages()
            .into_keys()
            .filter(|l| l == path)
            .collect();
        deleted.sort();
        result.extend(deleted.into_iter().take(1));
    }
    if result.is_empty() {
        bail!("no file history at {}", display_path(path));
    }
    follow_moves(history, &mut result);
    Ok(result)
}

/// Adds the lineage a v6 file was moved from: its first revision has no
/// parents and the same bytes as the last content of a deleted lineage.
fn follow_moves(history: &History, lineages: &mut Vec<String>) {
    let deleted = history.deleted_lineages();
    let mut index = 0;
    while index < lineages.len() && lineages.len() < 64 {
        let lineage = lineages[index].clone();
        index += 1;
        let roots: Vec<&String> = history
            .revs
            .iter()
            .filter(|(_, r)| r.lineage == lineage && r.parents.is_empty())
            .map(|(id, _)| id)
            .collect();
        for root in roots {
            let Some(hash) = history.revs[root].content.as_ref().map(|c| &c.hash) else {
                continue;
            };
            for (other, heads) in &deleted {
                if lineages.contains(other) {
                    continue;
                }
                let moved = heads.iter().any(|head| {
                    history.last_content(head).is_some_and(|c| {
                        history.revs[&c].content.as_ref().map(|x| &x.hash) == Some(hash)
                    }) && history.effective(head) <= history.effective(root)
                });
                if moved {
                    lineages.push(other.clone());
                }
            }
        }
    }
}

/// Classify a revision: deletion, first revision, restore (content seen in an
/// earlier, non-parent ancestor or parents all deletions) or modification.
fn kind(history: &History, id: &str) -> VersionKind {
    let rev = &history.revs[id];
    let Some(content) = &rev.content else {
        return VersionKind::Deleted;
    };
    let parents: Vec<_> = rev
        .parents
        .iter()
        .filter_map(|p| history.revs.get(p))
        .collect();
    if parents.is_empty() {
        return VersionKind::Created;
    }
    if parents.iter().all(|p| p.content.is_none()) {
        return VersionKind::Restored;
    }
    let direct: BTreeSet<_> = rev.parents.iter().cloned().collect();
    let earlier = history
        .ancestors(id)
        .into_iter()
        .filter(|a| !direct.contains(a))
        .any(|a| {
            history.revs[&a]
                .content
                .as_ref()
                .is_some_and(|c| c.hash == content.hash)
        });
    if earlier {
        VersionKind::Restored
    } else {
        VersionKind::Modified
    }
}

/// Purged trash entries take the bytes they deleted out of restore.
fn purged_bytes(history: &History) -> BTreeSet<String> {
    history
        .purged
        .iter()
        .filter_map(|id| history.last_content(id))
        .collect()
}

/// Every revision of the lineages of `path`, newest first, with kind, size,
/// author, whether it is current and whether its data is still restorable.
/// Used by `ops::read` for `Op::VersionsList`.
pub(crate) fn list(history: &History, path: &str) -> Result<Vec<VersionEntry>> {
    let lineages = lineages(history, path)?;
    let current: BTreeSet<String> = history.view_at(None)?.into_values().collect();
    let purged = purged_bytes(history);
    let mut ids: Vec<String> = history
        .revs
        .iter()
        .filter(|(_, r)| lineages.contains(&r.lineage))
        .map(|(id, _)| id.clone())
        .collect();
    history.newest_first(&mut ids);
    Ok(ids
        .into_iter()
        .map(|id| {
            let rev = &history.revs[&id];
            VersionEntry {
                path: display_path(&rev.lineage),
                kind: kind(history, &id),
                size: rev.content.as_ref().map_or(0, |c| c.size),
                time_unix: rev.time,
                author: Some(rev.author.clone()),
                current: current.contains(&id),
                restorable: history.payloads.contains_key(&id) && !purged.contains(&id),
                id,
            }
        })
        .collect())
}

/// Makes revision `id` of `path` current again (a new revision), or writes it
/// beside the file as `name (restored).ext` (`as_copy`).
pub(crate) fn restore_action(
    history: &History,
    path: &str,
    id: &str,
    as_copy: bool,
) -> Result<Action> {
    let entries = list(history, path)?;
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .with_context(|| format!("{id} is not a version of {}", display_path(path)))?;
    if entry.kind == VersionKind::Deleted {
        bail!("{id} is a deletion; choose a version with content");
    }
    if !entry.restorable {
        bail!("the data of version {id} is no longer available");
    }
    let lineage = &history.revs[id].lineage;
    let current = history.view_at(None)?;
    // A conflict copy path restores onto its file's own path.
    let home = if current
        .get(path)
        .is_some_and(|r| &history.revs[r].lineage == lineage)
    {
        lineage.clone()
    } else {
        path.to_owned()
    };
    if as_copy {
        let taken: Vec<String> = current.into_keys().collect();
        return Ok(Action::Put {
            path: unique_name(&home, &taken),
            rev: id.to_owned(),
            from: Some(home),
        });
    }
    if current.get(&home).map(String::as_str) == Some(id) {
        bail!("version {id} is already the current content");
    }
    Ok(Action::Put {
        path: home,
        rev: id.to_owned(),
        from: None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::graph::fixture::{history, rev, Fx};
    use super::*;

    fn sample() -> Fx {
        history(vec![
            ("v1", rev("f.txt", &[], Some("one"), "pc", Some(10))),
            ("v2", rev("f.txt", &["v1"], Some("two"), "pc", Some(20))),
            ("v3", rev("f.txt", &["v2"], None, "pc", Some(30))),
            ("v4", rev("f.txt", &["v3"], Some("two"), "pc", Some(40))),
            // Concurrent edits: a conflict.
            ("x", rev("f.txt", &["v4"], Some("x"), "PC-A", Some(50))),
            ("y", rev("f.txt", &["v4"], Some("y"), "PC-B", Some(51))),
        ])
    }

    #[test]
    fn newest_first_with_deletions_restores_and_conflict_branches() {
        let h = sample();
        let list = list(&h, "f.txt").unwrap();
        let ids: Vec<_> = list.iter().map(|e| h.name(&e.id)).collect();
        assert_eq!(ids, ["y", "x", "v4", "v3", "v2", "v1"]);
        let kinds: Vec<_> = list.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                VersionKind::Modified,
                VersionKind::Modified,
                VersionKind::Restored,
                VersionKind::Deleted,
                VersionKind::Modified,
                VersionKind::Created
            ]
        );
        // Original (v4) at the path and both branches as labelled copies are current.
        let current: Vec<_> = list
            .iter()
            .filter(|e| e.current)
            .map(|e| h.name(&e.id))
            .collect();
        assert_eq!(current, ["y", "x", "v4"]);
        // A conflict copy path lists the same file.
        let x = h.id("x");
        let copy = h
            .view_at(None)
            .unwrap()
            .into_iter()
            .find(|(_, id)| *id == x)
            .unwrap()
            .0;
        assert_eq!(super::list(&h, &copy).unwrap().len(), 6);
        assert!(super::list(&h, "missing.txt").is_err());
    }

    #[test]
    fn restore_writes_a_new_revision_or_a_copy() {
        let h = history(vec![
            ("v1", rev("f.txt", &[], Some("one"), "pc", Some(10))),
            ("v2", rev("f.txt", &["v1"], Some("two"), "pc", Some(20))),
            ("v3", rev("f.txt", &["v2"], None, "pc", Some(30))),
        ]);
        // Deleted file: versions still listed by its last path.
        assert_eq!(
            restore_action(&h, "f.txt", &h.id("v1"), false).unwrap(),
            Action::Put {
                path: "f.txt".into(),
                rev: h.id("v1"),
                from: None
            }
        );
        assert!(restore_action(&h, "f.txt", &h.id("v3"), false).is_err());
        let h = sample();
        assert!(restore_action(&h, "f.txt", &h.id("v4"), false).is_err());
        match restore_action(&h, "f.txt", &h.id("v1"), true).unwrap() {
            Action::Put { path, .. } => assert_eq!(path, "f (restored).txt"),
            other => panic!("{other:?}"),
        }
    }
}
