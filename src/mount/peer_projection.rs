//! Original-preserving projection for coordinator-free event namespaces.
//!
//! Concurrent heads of one path are not merged: each live head becomes a
//! peer-named file and the common original stays at the path, reported as a
//! [`Conflict`]. Entry point: [`project`], used by the v6 namespace, drive
//! generations and drive history.
use super::shared_model::{self, Event, Resolved};
use crate::prelude::*;

/// One concurrent head (branch) in a [`Conflict`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Branch {
    /// Event id of the head revision.
    pub revision: String,
    /// Worker (PC) that wrote it.
    pub worker: String,
    /// Device id that wrote it.
    pub device: String,
    /// Peer-named visible path the head was placed at; `None` for a deletion.
    pub file: Option<String>,
}
/// Concurrent heads at one path, shown in the GUI's conflict list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Conflict {
    /// Path that has more than one head.
    pub path: String,
    /// Visible paths where the common original(s) were placed.
    pub originals: Vec<String>,
    /// Every head of the path.
    pub branches: Vec<Branch>,
    /// True when no common ancestor with content exists (nothing kept at the
    /// original path).
    pub original_unavailable: bool,
    /// True when several maximal common ancestors exist (each original gets a
    /// peer-named path instead of the original path).
    pub ambiguous_original: bool,
}
/// Result of [`project`]: visible files plus the conflicts behind them.
pub(crate) struct Projection {
    /// Visible path -> resolved revision.
    pub files: BTreeMap<String, Resolved>,
    /// One entry per path with concurrent heads.
    pub conflicts: Vec<Conflict>,
}
/// `id` and all its transitive parents (including `id`).
fn ancestors(id: &str, events: &BTreeMap<String, Event>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut queue = vec![id.to_owned()];
    while let Some(id) = queue.pop() {
        if found.insert(id.clone()) {
            queue.extend(events[&id].parents.iter().cloned());
        }
    }
    found
}
/// First peer conflict path for `path`/`worker`/`id` (attempts `0..=limit`)
/// that overlaps neither reserved slots nor already placed files.
fn allocate(
    path: &str,
    worker: &str,
    id: &str,
    reserved: &[String],
    files: &BTreeMap<String, Resolved>,
    limit: usize,
) -> Result<String> {
    for attempt in 0..=limit {
        let candidate = shared_model::peer_path(path, worker, id, attempt)?;
        if !reserved
            .iter()
            .any(|p| shared_model::overlaps(p, &candidate))
            && !files.keys().any(|p| shared_model::overlaps(p, &candidate))
        {
            return Ok(candidate);
        }
    }
    bail!("cannot allocate peer conflict path")
}
/// Projects validated `events` into visible files: a single head stays at
/// its path; multiple heads become a [`Conflict`] with originals and
/// peer-named branches. Deleted-only paths are omitted.
pub(crate) fn project(events: &BTreeMap<String, Event>) -> Result<Projection> {
    // Validate IDs, DAG, portable paths and case/directory collisions first.
    shared_model::reduce(events)?;
    let referenced: BTreeSet<_> = events
        .values()
        .flat_map(|e| e.parents.iter().cloned())
        .collect();
    let mut heads: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, event) in events {
        if !referenced.contains(id) {
            heads
                .entry(event.path.clone())
                .or_default()
                .push(id.clone());
        }
    }
    // Only live logical slots reserve names; deleted files may become directories.
    let reserved: Vec<_> = heads
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| events[id].content.is_some()))
        .map(|(path, _)| path.clone())
        .collect();
    let mut result = Projection {
        files: BTreeMap::new(),
        conflicts: vec![],
    };
    for (path, ids) in heads {
        let live: Vec<_> = ids
            .iter()
            .filter(|id| events[*id].content.is_some())
            .collect();
        if live.is_empty() {
            continue;
        }
        if ids.len() == 1 {
            let id = &ids[0];
            result.files.insert(
                path,
                Resolved {
                    event_id: id.clone(),
                    event: events[id].clone(),
                },
            );
            continue;
        }
        let ancestry: Vec<_> = ids.iter().map(|id| ancestors(id, events)).collect();
        let mut common = ancestry[0].clone();
        for set in &ancestry[1..] {
            common.retain(|id| set.contains(id));
        }
        let mut older = BTreeSet::new();
        for id in &common {
            let mut prior = ancestors(id, events);
            prior.remove(id);
            older.extend(prior);
        }
        let maximal: Vec<_> = common.difference(&older).cloned().collect();
        let bases: Vec<_> = maximal
            .iter()
            .filter(|id| events[*id].content.is_some())
            .cloned()
            .collect();
        let mut group = Conflict {
            path: path.clone(),
            originals: vec![],
            branches: vec![],
            original_unavailable: bases.is_empty(),
            ambiguous_original: maximal.len() > 1,
        };
        for id in &bases {
            let target = if maximal.len() == 1 {
                path.clone()
            } else {
                allocate(
                    &path,
                    "original",
                    id,
                    &reserved,
                    &result.files,
                    events.len() + 1,
                )?
            };
            result.files.insert(
                target.clone(),
                Resolved {
                    event_id: id.clone(),
                    event: events[id].clone(),
                },
            );
            group.originals.push(target);
        }
        for id in ids {
            let event = &events[&id];
            let file = if event.content.is_some() {
                let target = allocate(
                    &path,
                    &event.worker,
                    &id,
                    &reserved,
                    &result.files,
                    events.len() + 1,
                )?;
                result.files.insert(
                    target.clone(),
                    Resolved {
                        event_id: id.clone(),
                        event: event.clone(),
                    },
                );
                Some(target)
            } else {
                None
            };
            group.branches.push(Branch {
                revision: id,
                worker: event.worker.clone(),
                device: event.device.clone(),
                file,
            });
        }
        result.conflicts.push(group);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(crate) fn event(worker: &str, path: &str, parents: Vec<String>, text: &str) -> Event {
        Event {
            version: 1,
            worker: worker.into(),
            device: format!("device-{worker}"),
            path: path.into(),
            parents,
            content: Some(super::super::shared_model::Content {
                hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
                size: 0,
                manifest: Manifest {
                    version: 2,
                    archive_id: format!("data-{}", blake3::hash(text.as_bytes())),
                    original_name: path.into(),
                    original_size: 0,
                    shard_size: 1024,
                    created_unix: 0,
                    content_root_blake3: crate::manifest::content_root_v2(0, 1024, &None, &[]),
                    coding: None,
                    shards: vec![],
                },
            }),
        }
    }
    fn map(events: Vec<Event>) -> BTreeMap<String, Event> {
        events.into_iter().map(|e| (e.id().unwrap(), e)).collect()
    }
    #[test]
    fn sibling_edits_keep_original_and_both_worker_variants_in_any_arrival_order() {
        let original = event("base", "report.txt", vec![], "original");
        let a = event("A", "report.txt", vec![original.id().unwrap()], "edit A");
        let b = event("B", "report.txt", vec![original.id().unwrap()], "edit B");
        let source = vec![original.clone(), a.clone(), b.clone()];
        let result = project(&map(source.clone())).unwrap();
        let reversed = project(&map(source.into_iter().rev().collect())).unwrap();
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.files["report.txt"].event_id, original.id().unwrap());
        assert_eq!(
            result.files.keys().collect::<Vec<_>>(),
            reversed.files.keys().collect::<Vec<_>>()
        );
        for edit in [&a, &b] {
            let path = result
                .files
                .iter()
                .find(|(_, r)| r.event_id == edit.id().unwrap())
                .unwrap()
                .0;
            assert!(path.contains(&format!("_{}+a-", edit.worker)));
            assert!(path.ends_with(".txt"));
        }
        assert_eq!(result.conflicts[0].branches.len(), 2);
        assert_eq!(result.conflicts[0].originals, ["report.txt"]);
    }
    #[test]
    fn independent_paths_and_sequential_edits_do_not_conflict() {
        let base = event("A", "a", vec![], "a");
        let edit = event("B", "a", vec![base.id().unwrap()], "a2");
        let other = event("B", "b", vec![], "b");
        let result = project(&map(vec![base, edit.clone(), other])).unwrap();
        assert!(result.conflicts.is_empty());
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files["a"].event_id, edit.id().unwrap());
    }
    #[test]
    fn multigeneration_and_resolution_racing_late_sibling_preserve_branches() {
        let o = event("O", "file", vec![], "O");
        let a = event("A", "file", vec![o.id().unwrap()], "A");
        let a2 = event("A", "file", vec![a.id().unwrap()], "A2");
        let b = event("B", "file", vec![o.id().unwrap()], "B");
        let mut events = map(vec![o.clone(), a, a2.clone(), b.clone()]);
        assert_eq!(
            project(&events).unwrap().files["file"].event_id,
            o.id().unwrap()
        );
        let resolved = event(
            "R",
            "file",
            vec![a2.id().unwrap(), b.id().unwrap()],
            "resolved",
        );
        events.insert(resolved.id().unwrap(), resolved);
        assert!(project(&events).unwrap().conflicts.is_empty());
        let late = event("C", "file", vec![o.id().unwrap()], "late");
        events.insert(late.id().unwrap(), late);
        let result = project(&events).unwrap();
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.files["file"].event_id, o.id().unwrap());
    }
    #[test]
    fn creation_and_delete_edit_are_explicit_without_invented_originals() {
        let a = event("same", "file", vec![], "A");
        let mut b = event("same", "file", vec![], "B");
        b.device = "other-device".into();
        let result = project(&map(vec![a.clone(), b])).unwrap();
        assert_eq!(result.files.len(), 2);
        assert!(!result.files.contains_key("file"));
        assert!(result.conflicts[0].original_unavailable);
        let edit = event("edit", "file", vec![a.id().unwrap()], "edit");
        let mut delete = event("delete", "file", vec![a.id().unwrap()], "delete");
        delete.content = None;
        let result = project(&map(vec![a.clone(), edit, delete])).unwrap();
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files["file"].event_id, a.id().unwrap());
        assert!(result.conflicts[0]
            .branches
            .iter()
            .any(|b| b.file.is_none()));
    }
    #[test]
    fn generated_alias_never_overwrites_real_file() {
        let o = event("O", "보고서.txt", vec![], "O");
        let a = event("same", "보고서.txt", vec![o.id().unwrap()], "A");
        let b = event("same", "보고서.txt", vec![o.id().unwrap()], "B");
        let alias = shared_model::peer_path("보고서.txt", "same", &a.id().unwrap(), 0).unwrap();
        let user = event("user", &alias, vec![], "user");
        let result = project(&map(vec![o, a.clone(), b, user.clone()])).unwrap();
        assert_eq!(result.files.len(), 4);
        assert_eq!(result.files[&alias].event_id, user.id().unwrap());
        assert!(result.files.values().any(|r| r.event_id == a.id().unwrap()));
    }
}

#[cfg(test)]
mod regression_tests {
    use super::tests::event;
    use super::*;
    fn map(values: Vec<Event>) -> BTreeMap<String, Event> {
        values.into_iter().map(|e| (e.id().unwrap(), e)).collect()
    }
    #[test]
    fn deleted_file_can_be_reused_as_directory_with_conflicting_child() {
        let base = event("A", "dir", vec![], "old file");
        let mut delete = event("A", "dir", vec![base.id().unwrap()], "delete");
        delete.content = None;
        let child = event("A", "dir/child", vec![], "child");
        let a = event("A", "dir/child", vec![child.id().unwrap()], "A");
        let b = event("B", "dir/child", vec![child.id().unwrap()], "B");
        let result = project(&map(vec![base, delete, child, a, b])).unwrap();
        assert_eq!(result.files.len(), 3);
        assert!(result.files.contains_key("dir/child"));
    }
    #[test]
    fn mixed_deleted_and_content_common_bases_remain_ambiguous() {
        let base = event("O", "file", vec![], "O");
        let a = event("A", "file", vec![base.id().unwrap()], "A");
        let mut d = event("D", "file", vec![base.id().unwrap()], "D");
        d.content = None;
        let x = event("X", "file", vec![a.id().unwrap(), d.id().unwrap()], "X");
        let y = event("Y", "file", vec![a.id().unwrap(), d.id().unwrap()], "Y");
        let result = project(&map(vec![base, a, d, x, y])).unwrap();
        assert!(result.conflicts[0].ambiguous_original);
        assert_eq!(result.conflicts[0].originals.len(), 1);
        assert!(!result.files.contains_key("file"));
    }
    #[test]
    fn delayed_opens_respect_latest_pending_replacement_and_deletion() {
        let root = tempfile::tempdir().unwrap();
        let mut drive = super::super::virtual_drive::fixture(root.path());
        drive.pool_sync_roots = vec!["crypt:pool".into()];
        drive.state.lock().unwrap().version = 6;
        let first = drive.begin("file").unwrap();
        std::fs::write(drive.spool_path(&first), b"first").unwrap();
        drive.seal(first).unwrap();
        let obsolete = drive.view().unwrap()["file"].clone();
        let second = drive.begin("file").unwrap();
        std::fs::write(drive.spool_path(&second), b"second").unwrap();
        drive.seal(second).unwrap();
        assert!(drive.pin_read("file", &obsolete).is_err());
        let current = drive.view().unwrap()["file"].clone();
        drive.delete("file").unwrap();
        assert!(drive.pin_read("file", &current).is_err());
    }
    #[test]
    fn peer_reads_fence_replaced_paths_until_remount_and_keep_true_original() {
        let root = tempfile::tempdir().unwrap();
        let mut drive = super::super::virtual_drive::fixture(root.path());
        drive.pool_sync_roots = vec!["crypt:pool".into()];
        let o = event("O", "file", vec![], "original");
        let a = event("A", "file", vec![o.id().unwrap()], "A");
        {
            let mut state = drive.state.lock().unwrap();
            state.version = 6;
            state.events = map(vec![o.clone(), a.clone()]).into();
            state.save(root.path()).unwrap();
        }
        let read_a = drive.view().unwrap()["file"].clone();
        drive.pin_read("file", &read_a).unwrap();
        let b = event("B", "file", vec![o.id().unwrap()], "B");
        drive
            .state
            .lock()
            .unwrap()
            .events
            .insert(b.id().unwrap(), b);
        let original = drive.view().unwrap()["file"].clone();
        assert_eq!(original.id(), o.id().unwrap());
        assert!(drive.pin_read("file", &original).is_err());
        assert!(drive.pin_read("file", &read_a).is_err());
        assert_eq!(read_a.id(), a.id().unwrap());
        // Fresh mount has no range-read lease; its first actual read supplies ancestry.
        drive.peer_read_pins.lock().unwrap().clear();
        drive.pin_read("file", &original).unwrap();
        let intent = drive.begin_observed("file", Some(&original)).unwrap();
        assert_eq!(intent.parents, vec![o.id().unwrap()]);
    }
}
